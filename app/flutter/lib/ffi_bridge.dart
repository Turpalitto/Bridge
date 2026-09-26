// Dart bindings for libdropbridge_ffi (C-ABI JSON surface).
//
// Rule: only commands/metadata/events cross this bridge — never file bytes.
// Blocking FFI calls (db_send, db_event) run on a worker isolate so the UI
// thread never stalls.
import 'dart:async';
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:isolate';

import 'package:ffi/ffi.dart';

// --- C signatures -----------------------------------------------------------

typedef _InitC = Pointer<Void> Function(Pointer<Utf8> cfgJson);
typedef _InitDart = Pointer<Void> Function(Pointer<Utf8> cfgJson);

typedef _InitWithKeyC = Pointer<Void> Function(Pointer<Utf8> cfgJson, Pointer<Uint8> keySeed);
typedef _InitWithKeyDart = Pointer<Void> Function(Pointer<Utf8> cfgJson, Pointer<Uint8> keySeed);

typedef _StrFnC = Pointer<Utf8> Function(Pointer<Void> h);
typedef _StrFnDart = Pointer<Utf8> Function(Pointer<Void> h);

typedef _SendC = Pointer<Utf8> Function(Pointer<Void> h, Pointer<Utf8> sendJson);
typedef _SendDart = Pointer<Utf8> Function(Pointer<Void> h, Pointer<Utf8> sendJson);

typedef _JoinC = Int32 Function(Pointer<Void> h, Pointer<Utf8> qr);
typedef _JoinDart = int Function(Pointer<Void> h, Pointer<Utf8> qr);

typedef _EventC = Pointer<Utf8> Function(Pointer<Void> h, Uint32 timeoutMs);
typedef _EventDart = Pointer<Utf8> Function(Pointer<Void> h, int timeoutMs);

typedef _WatcherC = Int32 Function(Pointer<Void> h, Pointer<Utf8> outbox);
typedef _WatcherDart = int Function(Pointer<Void> h, Pointer<Utf8> outbox);

typedef _SyncAddC = Int32 Function(Pointer<Void> h, Pointer<Utf8> folderJson);
typedef _SyncAddDart = int Function(Pointer<Void> h, Pointer<Utf8> folderJson);

typedef _SyncRemoveC = Int32 Function(Pointer<Void> h, Pointer<Utf8> path);
typedef _SyncRemoveDart = int Function(Pointer<Void> h, Pointer<Utf8> path);

typedef _ShutdownC = Void Function(Pointer<Void> h);
typedef _ShutdownDart = void Function(Pointer<Void> h);

typedef _LastErrorC = Pointer<Utf8> Function();
typedef _LastErrorDart = Pointer<Utf8> Function();

typedef _FreeC = Void Function(Pointer<Utf8> s);
typedef _FreeDart = void Function(Pointer<Utf8> s);

class DropBridgeError implements Exception {
  final String message;
  DropBridgeError(this.message);
  @override
  String toString() => 'DropBridgeError: $message';
}

/// Thin, mostly-synchronous wrapper. Long operations go through
/// [runBlocking] on a worker isolate.
class DropBridgeCore {
  DropBridgeCore._(this._lib);

  final DynamicLibrary _lib;
  Pointer<Void> _h = nullptr;

  late final _init = _lib.lookupFunction<_InitC, _InitDart>('db_init');
  late final _initWithKey = _lib.lookupFunction<_InitWithKeyC, _InitWithKeyDart>('db_init_with_key');
  late final _info = _lib.lookupFunction<_StrFnC, _StrFnDart>('db_info');
  late final _pairQr = _lib.lookupFunction<_StrFnC, _StrFnDart>('db_pair_qr');
  late final _join = _lib.lookupFunction<_JoinC, _JoinDart>('db_join');
  late final _devices = _lib.lookupFunction<_StrFnC, _StrFnDart>('db_devices');
  late final _send = _lib.lookupFunction<_SendC, _SendDart>('db_send');
  late final _event = _lib.lookupFunction<_EventC, _EventDart>('db_event');
  late final _watcher = _lib.lookupFunction<_WatcherC, _WatcherDart>('db_start_watcher');
  late final _syncFolderAdd = _lib.lookupFunction<_SyncAddC, _SyncAddDart>('db_sync_folder_add');
  late final _syncFolderRemove = _lib.lookupFunction<_SyncRemoveC, _SyncRemoveDart>('db_sync_folder_remove');
  late final _syncFoldersList = _lib.lookupFunction<_StrFnC, _StrFnDart>('db_sync_folders_list');
  late final _shutdown = _lib.lookupFunction<_ShutdownC, _ShutdownDart>('db_shutdown');
  late final _lastError = _lib.lookupFunction<_LastErrorC, _LastErrorDart>('db_last_error');
  late final _free = _lib.lookupFunction<_FreeC, _FreeDart>('db_free_string');

  static DropBridgeCore load() {
    final lib = Platform.isAndroid
        ? DynamicLibrary.open('libdropbridge_ffi.so')
        : Platform.isWindows
            ? DynamicLibrary.open('dropbridge_ffi.dll')
            : DynamicLibrary.open('libdropbridge_ffi.dylib');
    return DropBridgeCore._(lib);
  }

  String _lastErr() {
    final p = _lastError();
    return p == nullptr ? '' : p.toDartString();
  }

  Map<String, dynamic> _takeJson(Pointer<Utf8> p) {
    if (p == nullptr) throw DropBridgeError(_lastErr());
    final s = p.toDartString();
    _free(p);
    return jsonDecode(s) as Map<String, dynamic>;
  }

  Pointer<Utf8> _c(String s) => s.toNativeUtf8();

  Future<void> init({
    required String stateDir,
    required String receiveDir,
    required String name,
    String kind = 'phone',
    String relay = 'n0',
    int? port,
    bool announce = true,
    bool autoReceive = true,
    List<int>? hardwareKeySeed,
  }) async {
    final cfg = {
      'state_dir': stateDir,
      'receive_dir': receiveDir,
      'name': name,
      'kind': kind,
      'relay': relay,
      if (port != null) 'port': port,
      'announce': announce,
      'auto_receive': autoReceive,
    };
    final arg = _c(jsonEncode(cfg));
    Pointer<Void> h;
    if (hardwareKeySeed != null && hardwareKeySeed.length == 32) {
      final keyPtr = calloc<Uint8>(32);
      for (var i = 0; i < 32; i++) {
        keyPtr[i] = hardwareKeySeed[i];
      }
      h = _initWithKey(arg, keyPtr);
      calloc.free(keyPtr);
    } else {
      h = _init(arg);
    }
    calloc.free(arg);
    if (h == nullptr) throw DropBridgeError(_lastErr());
    _h = h;
  }

  Map<String, dynamic> info() => _takeJson(_info(_h));

  Future<Map<String, dynamic>> pairQr() => runBlocking(() => _takeJson(_pairQr(_h)));

  Future<void> join(String qr) async {
    final arg = _c(qr);
    final r = _join(_h, arg);
    calloc.free(arg);
    if (r != 0) throw DropBridgeError(_lastErr());
  }

  Future<List<dynamic>> devices() async {
    final j = await runBlocking(() => _takeJson(_devices(_h)));
    return (j['devices'] as List?) ?? const [];
  }

  Future<Map<String, dynamic>> send({
    required String peer,
    required List<String> paths,
    int? session,
  }) {
    return runBlocking(() {
      final arg = _c(jsonEncode({
        'peer': peer,
        'paths': paths,
        'session': session,
      }));
      try {
        return _takeJson(_send(_h, arg));
      } finally {
        calloc.free(arg);
      }
    });
  }

  Future<void> startWatcher(String outbox) async {
    final arg = _c(outbox);
    final r = _watcher(_h, arg);
    calloc.free(arg);
    if (r != 0) throw DropBridgeError(_lastErr());
  }

  Future<void> addSyncFolder(String path, {String target = 'auto'}) async {
    final arg = _c(jsonEncode({'path': path, 'target': target}));
    try {
      final r = _syncFolderAdd(_h, arg);
      if (r != 0) throw DropBridgeError(_lastErr());
    } finally {
      calloc.free(arg);
    }
  }

  Future<void> removeSyncFolder(String path) async {
    final arg = _c(path);
    try {
      final r = _syncFolderRemove(_h, arg);
      if (r != 0) throw DropBridgeError(_lastErr());
    } finally {
      calloc.free(arg);
    }
  }

  Future<List<Map<String, dynamic>>> listSyncFolders() async {
    final j = await runBlocking(() => _takeJson(_syncFoldersList(_h)));
    final list = (j['folders'] as List?) ?? const [];
    return list.cast<Map<String, dynamic>>();
  }

  /// One event, waiting up to [timeoutMs]. Returns null on timeout.
  Map<String, dynamic>? pollEventSync({int timeoutMs = 1000}) {
    final p = _event(_h, timeoutMs);
    if (p == nullptr) return null; // timeout (or error — check _lastErr)
    final s = p.toDartString();
    _free(p);
    return jsonDecode(s) as Map<String, dynamic>;
  }

  /// Event stream backed by a worker isolate long-polling db_event.
  Stream<Map<String, dynamic>> events() {
    final controller = StreamController<Map<String, dynamic>>();
    final recv = RawReceivePort();
    final handle = _h.address;

    recv.handler = (msg) {
      if (msg == null) {
        controller.close();
        recv.close();
        return;
      }
      controller.add(Map<String, dynamic>.from(msg as Map));
    };

    Isolate.spawn((SendPort out) {
      final core = DropBridgeCore.load();
      core._h = Pointer.fromAddress(handle);
      while (true) {
        final ev = core.pollEventSync(timeoutMs: 2000);
        if (ev != null) out.send(ev);
      }
    }, recv.sendPort);

    return controller.stream;
  }

  /// Run a blocking FFI closure off the UI isolate.
  Future<T> runBlocking<T>(T Function() body) async {
    return await Isolate.run(body);
  }

  void shutdown() {
    if (_h != nullptr) {
      _shutdown(_h);
      _h = nullptr;
    }
  }
}
