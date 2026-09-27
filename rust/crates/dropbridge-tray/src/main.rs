//! DropBridge Windows tray resident (spec §31).
//!
//! Responsibilities:
//! * live in the system tray with the DropBridge menu,
//! * supervise the `dropbridge daemon` process (the actual engine) and restart
//!   it with backoff if it dies,
//! * open the receive/send folders,
//! * show the pairing QR in a console window,
//! * (un)register autostart (spec §71) — no admin rights needed,
//! * graceful daemon shutdown before exiting.
//!
//! All file transfer logic lives in the core binary — the tray never touches
//! file bytes or keys.
//!
//! Architecture notes (Windows specifics that are easy to get wrong):
//! * `windows_subsystem = "windows"` — a tray resident must not own a console
//!   window, so failures are reported through a message box, not `eprintln!`.
//! * The tray pumps the Win32 message queue on its own thread. `tray-icon`
//!   creates the hidden message window on the calling thread and installs a
//!   `WndProc`; `muda` routes menu clicks by subclassing that window. Without a
//!   pump the window procedure never runs, so a right-click never opens the
//!   menu: the icon is visible but completely inert.
//! * Everything (menu, tray icon, daemon supervision) therefore lives on one
//!   thread that interleaves `PeekMessage` + `MenuEvent` draining.
//! * The engine's stdout/stderr go to a rotating log file, not to the void, so
//!   a daemon that dies at startup leaves evidence behind.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "dropbridge-tray is a Windows shell; on this platform run `dropbridge daemon` directly."
    );
}

#[cfg(windows)]
mod win {
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use anyhow::{anyhow, Context, Result};
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIconBuilder};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, FALSE, HANDLE, TRUE,
    };
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MessageBoxW, PeekMessageW, TranslateMessage, MB_ICONERROR, MB_OK, MSG,
        PM_REMOVE,
    };

    const ID_SEND: &str = "send";
    const ID_PAIR: &str = "pair";
    const ID_OPEN_RECEIVED: &str = "open_received";
    const ID_OPEN_TOPHONE: &str = "open_tophone";
    const ID_DEVICES: &str = "devices";
    const ID_AUTOSTART: &str = "autostart";
    const ID_PAUSE: &str = "pause";
    const ID_QUIT: &str = "quit";

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

    /// How long the engine gets to honour a polite stop before it is killed.
    const DAEMON_STOP_TIMEOUT: Duration = Duration::from_secs(10);
    /// Main-loop period: bounds both menu latency and supervisor latency.
    const TICK: Duration = Duration::from_millis(500);
    /// Rotate the log once it passes this size (one previous generation kept).
    const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;
    /// Backoff ceiling for restarting a crashed engine.
    const MAX_BACKOFF: Duration = Duration::from_secs(60);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn user_dir(sub: &str) -> PathBuf {
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
        let p = PathBuf::from(home).join("DropBridge").join(sub);
        let _ = std::fs::create_dir_all(&p);
        p
    }

    /// Absolute path of the engine binary that ships next to the tray.
    ///
    /// There is deliberately no bare-`dropbridge.exe` fallback any more: a bare
    /// name is resolved against the current working directory, so a tray
    /// started from an untrusted folder could be tricked into launching a
    /// planted binary. A missing sibling is a hard error instead.
    pub fn core_binary() -> Result<PathBuf> {
        let exe = std::env::current_exe().context("cannot resolve the tray executable path")?;
        let dir = exe
            .parent()
            .ok_or_else(|| anyhow!("the tray executable has no parent directory"))?;
        let cand = dir.join("dropbridge.exe");
        if cand.is_file() {
            Ok(cand)
        } else {
            Err(anyhow!(
                "dropbridge.exe was not found next to dropbridge-tray.exe (looked in {}). \
                 Keep both binaries in the same folder.",
                dir.display()
            ))
        }
    }

    // ---------------------------------------------------------------- logging

    #[derive(Clone)]
    struct LogFile(Arc<Mutex<std::fs::File>>);

    impl std::io::Write for LogFile {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            match self.0.lock() {
                Ok(mut f) => f.write(buf),
                Err(_) => Ok(buf.len()),
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            match self.0.lock() {
                Ok(mut f) => f.flush(),
                Err(_) => Ok(()),
            }
        }
    }

    pub fn log_path() -> PathBuf {
        user_dir("logs").join("dropbridge.log")
    }

    fn rotate_if_large(path: &Path) {
        if std::fs::metadata(path)
            .map(|m| m.len() > MAX_LOG_BYTES)
            .unwrap_or(false)
        {
            let old = path.with_extension("log.1");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(path, &old);
        }
    }

    /// Open an append handle on the shared log file.
    fn open_log_append() -> Option<std::fs::File> {
        let path = log_path();
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()
    }

    /// Send the tray's own tracing output to the same log file.
    fn init_logging() {
        let path = log_path();
        rotate_if_large(&path);
        let Some(file) = open_log_append() else {
            return;
        };
        let sink = LogFile(Arc::new(Mutex::new(file)));
        let _ = tracing_subscriber::fmt()
            .with_writer(move || sink.clone())
            .with_ansi(false)
            .with_target(true)
            .try_init();
    }

    // ------------------------------------------------------------- ui helpers

    /// A GUI-subsystem binary has nowhere to print to, so fatal errors go
    /// through a message box; without this the app fails completely silently.
    pub fn alert(caption: &str, text: &str) {
        let (c, t) = (wide(caption), wide(text));
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                t.as_ptr(),
                c.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    // -------------------------------------------------------- single instance

    struct SingleInstance(HANDLE);

    impl Drop for SingleInstance {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// One tray per session: a second copy would fight over the Run key and
    /// start a second engine, which is confusing at best.
    fn acquire_single_instance() -> Result<SingleInstance> {
        use windows_sys::Win32::System::Threading::CreateMutexW;
        let name = wide("Local\\DropBridgeTray");
        let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
        if handle.is_null() {
            return Err(anyhow!("CreateMutexW failed (error {})", unsafe {
                GetLastError()
            }));
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                CloseHandle(handle);
            }
            return Err(anyhow!(
                "DropBridge is already running — use the icon in the notification area"
            ));
        }
        Ok(SingleInstance(handle))
    }

    // ------------------------------------------------------------------ icon

    /// 32×32 procedural icon: calm rounded square with a bridge bar.
    pub fn make_icon() -> Result<Icon> {
        let size = 32u32;
        let mut rgba = vec![0u8; (size * size * 4) as usize];
        for y in 0..size {
            for x in 0..size {
                let i = ((y * size + x) * 4) as usize;
                let inside = (4..size - 4).contains(&x) && (4..size - 4).contains(&y);
                let bar = (14..18).contains(&y) && (6..size - 6).contains(&x);
                if bar {
                    rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                } else if inside {
                    rgba[i..i + 4].copy_from_slice(&[36, 99, 235, 255]);
                }
            }
        }
        Icon::from_rgba(rgba, size, size).context("cannot build the tray icon")
    }

    // ------------------------------------------------------------- autostart

    pub fn autostart_key() -> &'static str {
        r"Software\Microsoft\Windows\CurrentVersion\Run"
    }

    fn open_run_key(write: bool) -> Option<HANDLE> {
        use windows_sys::Win32::System::Registry::{
            RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE,
        };
        let key_path = wide(autostart_key());
        let access = if write {
            KEY_READ | KEY_WRITE
        } else {
            KEY_READ
        };
        let mut hk: HKEY = std::ptr::null_mut();
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_path.as_ptr(), 0, access, &mut hk) };
        if rc != 0 {
            None
        } else {
            Some(hk as HANDLE)
        }
    }

    /// Is autostart currently registered?
    pub fn autostart_enabled() -> bool {
        use windows_sys::Win32::System::Registry::{RegCloseKey, RegQueryValueExW};
        let value_name = wide("DropBridge");
        let Some(hk) = open_run_key(false) else {
            return false;
        };
        let mut len: u32 = 0;
        let present = unsafe {
            RegQueryValueExW(
                hk,
                value_name.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut len,
            ) == 0
        };
        unsafe {
            RegCloseKey(hk);
        }
        present
    }

    /// Toggle the HKCU Run key (no admin required, spec §71). Returns the new
    /// state, or `None` if the registry could not be read or written.
    pub fn toggle_autostart() -> Option<bool> {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, REG_SZ,
        };
        let value_name = wide("DropBridge");
        let hk = open_run_key(true)?;
        let mut len: u32 = 0;
        let exists = unsafe {
            RegQueryValueExW(
                hk,
                value_name.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut len,
            ) == 0
        };
        let new_state = !exists;
        if exists {
            unsafe {
                RegDeleteValueW(hk, value_name.as_ptr());
            }
        } else {
            let exe = std::env::current_exe().unwrap_or_default();
            let data = wide(&format!("\"{}\"", exe.display()));
            unsafe {
                RegSetValueExW(
                    hk,
                    value_name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr() as *const u8,
                    (data.len() * 2) as u32,
                );
            }
        }
        unsafe {
            RegCloseKey(hk);
        }
        Some(new_state)
    }

    // --------------------------------------------------------- message pump

    /// Drain the Win32 message queue for the tray's hidden message window.
    ///
    /// `GetMessage` would block, so `PeekMessage` is used: the same thread
    /// must also service menu events and the daemon supervisor.
    fn pump_messages() {
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    // -------------------------------------------------------- daemon control

    /// The engine plus a restart policy: it is restarted with exponential
    /// backoff when it exits on its own, so a crash loop cannot spin the CPU.
    struct Daemon {
        child: Option<Child>,
        backoff: Duration,
        retry_at: Option<Instant>,
        restarts: u32,
    }

    impl Daemon {
        fn new() -> Self {
            Self {
                child: None,
                backoff: Duration::from_secs(1),
                retry_at: None,
                restarts: 0,
            }
        }

        /// Reap the engine if it exited; returns whether it is running.
        fn running(&mut self) -> bool {
            let exited = match self.child.as_mut() {
                Some(c) => c.try_wait().ok().flatten().is_some(),
                None => false,
            };
            if exited {
                self.child = None;
            }
            self.child.is_some()
        }

        fn start(&mut self, core: &Path) -> Result<()> {
            let (out, err) = match (open_log_append(), open_log_append()) {
                (Some(o), Some(e)) => (Stdio::from(o), Stdio::from(e)),
                _ => (Stdio::null(), Stdio::null()),
            };
            let child = Command::new(core)
                .arg("daemon")
                .current_dir(core.parent().unwrap_or(Path::new(".")))
                .creation_flags(CREATE_NO_WINDOW)
                .stdout(out)
                .stderr(err)
                .spawn()
                .with_context(|| format!("cannot start the engine at {}", core.display()))?;
            tracing::info!(pid = child.id(), "engine started");
            self.child = Some(child);
            self.retry_at = None;
            self.backoff = Duration::from_secs(1);
            Ok(())
        }

        /// Start the engine now, or once the backoff window has elapsed.
        fn supervise(&mut self, core: &Path) {
            if self.running() {
                return;
            }
            let now = Instant::now();
            if let Some(at) = self.retry_at {
                if now < at {
                    return;
                }
            }
            self.restarts = self.restarts.saturating_add(1);
            let wait = self.backoff;
            match self.start(core) {
                Ok(()) => {
                    self.restarts = 0;
                }
                Err(e) => {
                    tracing::error!(error = %e, "the engine could not be started");
                    self.child = None;
                    self.retry_at = Some(now + wait);
                    self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
                }
            }
        }

        /// Polite stop first, hard kill only if the engine ignores it.
        ///
        /// The engine runs with `CREATE_NO_WINDOW`, so it owns no top-level
        /// window and `taskkill` without `/F` has nothing to close. A console
        /// control event is the only polite stop available: we attach to the
        /// engine's console, broadcast the event, and let the engine's
        /// `SetConsoleCtrlHandler` unwind its event loop and close the node so
        /// in-flight transfers finish.
        fn stop(&mut self) {
            let Some(mut child) = self.child.take() else {
                return;
            };
            let pid = child.id();
            for (event, name) in [(CTRL_BREAK_EVENT, "CTRL_BREAK"), (CTRL_C_EVENT, "CTRL_C")] {
                if !request_console_ctrl_event(pid, event) {
                    tracing::warn!(
                        pid,
                        event = name,
                        "cannot deliver the console control event"
                    );
                    continue;
                }
                if wait_for_exit(&mut child, DAEMON_STOP_TIMEOUT) {
                    tracing::info!(pid, event = name, "engine stopped");
                    return;
                }
                tracing::warn!(
                    pid,
                    event = name,
                    "engine ignored the event; trying the next one"
                );
            }
            tracing::warn!(pid, "the engine ignored every polite stop; terminating it");
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Deliver a console control event to every process sharing `pid`'s console.
    ///
    /// Returns `false` when the console could not be reached at all (the
    /// engine has none, or it already died).
    fn request_console_ctrl_event(pid: u32, event: u32) -> bool {
        use windows_sys::Win32::System::Console::{
            AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
        };
        unsafe {
            if AttachConsole(pid) == FALSE {
                if GetLastError() == ERROR_ACCESS_DENIED {
                    // A console is already attached to the tray (it inherits
                    // none, but a leaked attachment from an earlier call
                    // would); drop it and retry once.
                    let _ = FreeConsole();
                }
                if AttachConsole(pid) == FALSE {
                    return false;
                }
            }
            // Inherit Ctrl+C/Ctrl+Break while attached so the broadcast does
            // not take the tray down with the engine.
            let _ = SetConsoleCtrlHandler(None, TRUE);
            let sent = GenerateConsoleCtrlEvent(event, 0) != FALSE;
            let _ = SetConsoleCtrlHandler(None, FALSE);
            let _ = FreeConsole();
            sent
        }
    }

    /// Poll until `child` exits or `timeout` elapses. `true` if it exited.
    fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                Ok(None) => return false,
                Err(_) => return false,
            }
        }
    }

    // -------------------------------------------------------- core consoles

    /// Launch `dropbridge <args>` in its own console window.
    ///
    /// Non-blocking on purpose: the old implementation called `.status()`, which
    /// froze the whole (single-threaded) tray until the child exited.
    fn spawn_core_console(core: &Path, args: &[&str], children: &mut Vec<Child>) {
        match Command::new(core)
            .args(args)
            .current_dir(core.parent().unwrap_or(Path::new(".")))
            .creation_flags(CREATE_NEW_CONSOLE)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => children.push(child),
            Err(e) => tracing::error!(?args, error = %e, "cannot open the engine console"),
        }
    }

    /// Drop handles of console windows the user already closed.
    fn reap_core_consoles(children: &mut Vec<Child>) {
        children.retain_mut(|c| !matches!(c.try_wait(), Ok(Some(_))));
    }

    // ----------------------------------------------------------------- main

    pub fn run() -> Result<()> {
        let _instance = acquire_single_instance()?;
        init_logging();

        let core = match core_binary() {
            Ok(c) => c,
            Err(e) => {
                // A missing engine is the single most likely reason a fresh
                // install "does nothing", so say so loudly.
                alert(
                    "DropBridge",
                    &format!("{e}\n\nLog file: {}", log_path().display()),
                );
                return Err(e);
            }
        };

        let menu = Menu::new();
        let send = MenuItem::with_id(ID_SEND, "Send files…", true, None);
        let pair = MenuItem::with_id(ID_PAIR, "Pair new device…", true, None);
        let open_recv = MenuItem::with_id(ID_OPEN_RECEIVED, "Open received files", true, None);
        let open_send = MenuItem::with_id(ID_OPEN_TOPHONE, "Open “To Phone” folder", true, None);
        let devices = MenuItem::with_id(ID_DEVICES, "Devices", true, None);
        let autostart = CheckMenuItem::with_id(
            ID_AUTOSTART,
            "Start with Windows",
            true,
            autostart_enabled(),
            None,
        );
        let pause = CheckMenuItem::with_id(ID_PAUSE, "Pause receiving", true, false, None);
        let quit = MenuItem::with_id(ID_QUIT, "Quit", true, None);
        menu.append_items(&[
            &send,
            &PredefinedMenuItem::separator(),
            &pair,
            &open_recv,
            &open_send,
            &devices,
            &PredefinedMenuItem::separator(),
            &autostart,
            &pause,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("DropBridge — pair once, drop anywhere")
            .with_icon(make_icon()?)
            .build()?;

        let mut daemon = Daemon::new();
        let mut consoles: Vec<Child> = Vec::new();
        let mut paused = false;
        let mut quit = false;

        while !quit {
            // 1. let the tray window procedure run (this is what makes the menu open)
            pump_messages();

            // 2. service menu clicks
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                match ev.id().as_ref() {
                    ID_SEND => spawn_core_console(&core, &["send", "auto"], &mut consoles),
                    ID_PAIR => spawn_core_console(&core, &["pair"], &mut consoles),
                    ID_OPEN_RECEIVED => {
                        let _ = open::that(user_dir("From Phone"));
                    }
                    ID_OPEN_TOPHONE => {
                        let _ = open::that(user_dir("To Phone"));
                    }
                    ID_DEVICES => spawn_core_console(&core, &["devices"], &mut consoles),
                    ID_AUTOSTART => match toggle_autostart() {
                        Some(on) => {
                            autostart.set_checked(on);
                            tracing::info!(autostart = on, "autostart toggled");
                        }
                        None => {
                            tracing::error!("cannot read or write the autostart registry value")
                        }
                    },
                    ID_PAUSE => {
                        paused = !paused;
                        pause.set_checked(paused);
                        if paused {
                            // Stop the engine; in-flight transfers get the grace
                            // period. Resuming restarts it via the supervisor.
                            daemon.stop();
                        }
                    }
                    ID_QUIT => quit = true,
                    _ => {}
                }
            }

            // 3. keep the engine alive, 4. release closed console handles
            if !paused {
                daemon.supervise(&core);
            }
            reap_core_consoles(&mut consoles);

            std::thread::sleep(TICK);
        }

        // Engine first, then the icon: dropping the tray sends
        // Shell_NotifyIcon(NIM_DELETE) so Explorer does not keep a ghost entry.
        daemon.stop();
        drop(tray);
        Ok(())
    }
}

#[cfg(windows)]
fn main() {
    if let Err(e) = win::run() {
        win::alert("DropBridge", &e.to_string());
        std::process::exit(1);
    }
}
