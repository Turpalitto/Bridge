//! Graceful shutdown signalling for long-running commands.
//!
//! On Unix this is plain `SIGTERM`/`SIGINT` handling. On Windows there is no
//! equivalent stream signal: the only way a process is asked politely to stop
//! is a *console control event* delivered through `SetConsoleCtrlHandler`.
//! That matters here because the tray app supervises `dropbridge daemon` and
//! stops it with `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)` — without a
//! handler the daemon is killed hard and every in-flight transfer is lost.
//!
//! The Win32 handler is invoked on a thread the OS creates for the event, so
//! it may not touch the tokio runtime. It only flips an atomic flag; a
//! lightweight async poller turns that flag into an awaitable future.

/// What asked the process to stop.
///
/// Each platform only ever constructs a subset: `Term`/`CtrlC` on Unix,
/// `CtrlC`/`ConsoleClose` on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ShutdownKind {
    CtrlC,
    Term,
    ConsoleClose,
}

impl ShutdownKind {
    pub fn message(self) -> &'static str {
        match self {
            ShutdownKind::CtrlC => "⏹ shutdown requested (Ctrl+C) — finishing up…",
            ShutdownKind::Term => "⏹ terminated (SIGTERM) — finishing up…",
            ShutdownKind::ConsoleClose => "⏹ console close requested — finishing up…",
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::ShutdownKind;
    use std::future::Future;
    use std::pin::Pin;

    /// Unix uses tokio's signal machinery, which needs no extra installation.
    pub fn install() {}

    /// Resolves on SIGINT or SIGTERM.
    pub fn wait() -> Pin<Box<dyn Future<Output = ShutdownKind> + Send>> {
        Box::pin(async {
            use tokio::signal::unix::{signal, SignalKind};
            let mut term = match signal(SignalKind::terminate()) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("cannot install SIGTERM handler: {e}");
                    None
                }
            };
            let mut int = match signal(SignalKind::interrupt()) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("cannot install SIGINT handler: {e}");
                    None
                }
            };
            loop {
                if let Some(s) = int.as_mut() {
                    tokio::select! {
                        _ = s.recv() => return ShutdownKind::CtrlC,
                        _ = async { match term.as_mut() {
                            Some(t) => { t.recv().await; }
                            None => std::future::pending::<()>().await,
                        } } => return ShutdownKind::Term,
                    }
                } else if let Some(s) = term.as_mut() {
                    s.recv().await;
                    return ShutdownKind::Term;
                } else {
                    std::future::pending::<()>().await;
                }
            }
        })
    }
}

#[cfg(windows)]
mod imp {
    use super::ShutdownKind;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT,
    };

    /// 0 = nothing, 1 = Ctrl+C / Ctrl+Break, 2 = console close / logoff /
    /// shutdown.
    static REQUESTED: AtomicU8 = AtomicU8::new(0);

    unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
        let code = match ctrl_type {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => 1,
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => 2,
            _ => return FALSE,
        };
        REQUESTED.store(code, Ordering::SeqCst);
        // Returning TRUE stops the system from killing us on the spot: the
        // daemon needs a few seconds to close the node, flush the journal and
        // release the sqlite handle.
        TRUE
    }

    /// Installs the console control handler. Detaches any previous handler
    /// first (passing `None` with `add = FALSE`), which keeps repeated calls
    /// from stacking duplicate handlers.
    pub fn install() {
        unsafe {
            let _ = SetConsoleCtrlHandler(None, FALSE);
            if SetConsoleCtrlHandler(Some(handler), TRUE) == FALSE {
                tracing::warn!(
                    "SetConsoleCtrlHandler failed; the daemon will only stop on a hard kill"
                );
            }
        }
    }

    #[allow(dead_code)]
    pub fn requested() -> bool {
        REQUESTED.load(Ordering::SeqCst) != 0
    }

    /// Resolves once a console control event arrives. Polls the flag the
    /// handler sets, because the handler runs on an OS-owned thread that must
    /// not touch the tokio runtime.
    pub fn wait() -> Pin<Box<dyn Future<Output = ShutdownKind> + Send>> {
        Box::pin(async {
            loop {
                match REQUESTED.load(Ordering::SeqCst) {
                    1 => return ShutdownKind::CtrlC,
                    2 => return ShutdownKind::ConsoleClose,
                    _ => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        })
    }
}

/// Installs the platform shutdown handler. Call once, early in `main`.
pub fn install() {
    imp::install();
}

/// Resolves when the process is asked to stop.
pub fn wait() -> std::pin::Pin<Box<dyn std::future::Future<Output = ShutdownKind> + Send>> {
    imp::wait()
}
