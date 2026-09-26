// Dart bindings for libdropbridge_ffi (C-ABI JSON surface).
//
// Rule: only commands/metadata/events cross this bridge — never file bytes.
// Blocking FFI calls (db_send, db_event, db_devices, ...) run on a worker
// isolate so the UI thread never stalls.
//
// IMPORTANT: Dart isolates can only exchange "sendable" values (primitives,
// Strings, Lists, Maps, SendPorts). A `DynamicLibrary`, `Pointer` wrapper
// closures capturing `this` are NOT sendable — passing them to
// `Isolate.run`/`Isolate.spawn` throws:
//
//   Invalid argument(s): Illegal argument in isolate message:
//   (object is a DynamicLibrary)
//
// So every worker entry point below is a TOP-LEVEL function and only `int`
// (handle address) + `String` payloads are captured. The library is re-opened
// INSIDE the isolate. `db_last_error` is thread-local in Rust, therefore the
// error string is always read on the same thread/isolate right after the call.
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

// --- Top-level isolate helpers (must stay top-level!) -----------------------

String _libFileName() {
  if (Platform.isAndroid) return 'libdropbridge_ffi.so';
  if (Platform.isWindows) return 'dropbridge_ffi.dll';
  return 'libdropbridge_ffi.dylib';
}

DynamicLibrary _openLibForIsolate() =>
    DynamicLibrary.open(_libFileName());

String _lastErrIsolate(DynamicLibrary lib) {
  try {
    final fn = lib.lookupFunction<_LastErrorC, _LastErrorDart>('db_last_error');
    final p = fn();
    if (p == nullptr) return '';
    return p.toDartString();
  } catch (_) {
    return '';
  }
}

Map<String, dynamic> _takeJsonIsolate(DynamicLibrary lib, Pointer<Utf8> p) {
  if (p == nullptr) throw DropBridgeError(_lastErrIsolate(lib));
  final free = lib.lookupFunction<_FreeC, _FreeDart>('db_free_string');
  final s = p.toDartString();
  free(p);
  final v = jsonDecode(s);
  if (v is Map) return Map<String, dynamic>.from(v);
  return {'v': 1, 'value': v};
}

Map<String, dynamic> _devicesSync(int handleAddr) {
  final lib = _openLibForIsolate();
  final fn = lib.lookupFunction<_StrFnC, _StrFnDart>('db_devices');
  return _takeJsonIsolate(lib, fn(Pointer.fromAddress(handleAddr)));
}

Map<String, dynamic> _pairQrSync(int handleAddr) {
  final lib = _openLibForIsolate();
  final fn = lib.lookupFunction<_StrFnC, _StrFnDart>('db_pair_qr');
  return _takeJsonIsolate(lib, fn(Pointer.fromAddress(handleAddr)));
}

Map<String, dynamic> _syncListSync(int handleAddr) {
  final lib = _openLibForIsolate();
  final fn =
      lib.lookupFunction<_StrFnC, _StrFnDart>('db_sync_folders_list');
  return _takeJsonIsolate(lib, fn(Pointer.fromAddress(handleAddr)));
}

Map<String, dynamic> _sendSync(int handleAddr, String reqJson) {
  final lib = _openLibForIsolate();
  final fn = lib.lookupFunction<_SendC, _SendDart>('db_send');
  final arg = reqJson.toNativeUtf8();
  try {
    return _takeJsonIsolate(
        lib, fn(Pointer.fromAddress(handleAddr), arg));
  } finally {
    calloc.free(arg);
  }
}

void _joinSync(int handleAddr, String qr) {
  final lib = _openLibForIsolate();
  final fn = lib.lookupFunction<_JoinC, _JoinDart>('db_join');
  final arg = qr.toNativeUtf8();
  try {
    final r = fn(Pointer.fromAddress(handleAddr), arg);
    if (r != 0) throw DropBridgeError(_lastErrIsolate(lib));
  } finally {
    calloc.free(arg);
  }
}

/// Entry for the long-polling event isolate.
/// message = [SendPort, int handleAddr]
void _eventsEntry(List<dynamic> message) {
  final out = message[0] as SendPort;
  final handleAddr = message[1] as int;
  final lib = _openLibForIsolate();
  final eventFn = lib.lookupFunction<_EventC, _EventDart>('db_event');
  final freeFn = lib.lookupFunction<_FreeC, _FreeDart>('db_free_string');
  final h = Pointer<Void>.fromAddress(handleAddr);
  while (true) {
    Pointer<Utf8> p;
    try {
      p = eventFn(h, 2000);
    } catch (_) {
      continue;
    }
    if (p == nullptr) continue; // timeout
    String s;
    try {
      s = p.toDartString();
    } finally {
      try {
        freeFn(p);
      } catch (_) {}
    }
    try {
      final v = jsonDecode(s);
      if (v is Map) {
        out.send(Map<String, dynamic>.from(v));
      }
    } catch (_) {
      // ignore malformed event
    }
  }
}

/// Thin wrapper around the FFI handle. Fast/short calls run on the calling
/// isolate; blocking calls are dispatched to a worker isolate via top-level
/// helpers above (only `int` + `String` cross the isolate boundary).
class DropBridgeCore {
  DropBridgeCore._(this._lib);

  final DynamicLibrary _lib;
  Pointer<Void> _h = nullptr;

  late final _init = _lib.lookupFunction<_InitC, _InitDart>('db_init');
  late final _initWithKey = _lib.lookupFunction<_InitWithKeyC, _InitWithKeyDart>('db_init_with_key');
  late final _info = _lib.lookupFunction<_StrFnC, _StrFnDart>('db_info');
  late final _event = _lib.lookupFunction<_EventC, _EventDart>('db_event');
  late final _watcher = _lib.lookupFunction<_WatcherC, _WatcherDart>('db_start_watcher');
  late final _syncFolderAdd = _lib.lookupFunction<_SyncAddC, _SyncAddDart>('db_sync_folder_add');
  late final _syncFolderRemove = _lib.lookupFunction<_SyncRemoveC, _SyncRemoveDart>('db_sync_folder_remove');
  late final _shutdown = _lib.lookupFunction<_ShutdownC, _ShutdownDart>('db_shutdown');
  late final _lastError = _lib.lookupFunction<_LastErrorC, _LastErrorDart>('db_last_error');
  late final _free = _lib.lookupFunction<_FreeC, _FreeDart>('db_free_string');

  static DropBridgeCore load() {
    return DropBridgeCore._(DynamicLibrary.open(_libFileName()));
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

  Future<Map<String, dynamic>> pairQr() {
    final h = _h.address;
    // Only sendable `int` is captured — library is re-opened in the isolate.
    return Isolate.run(() => _pairQrSync(h));
  }

  Future<void> join(String qr) {
    final h = _h.address;
    final req = qr;
    return Isolate.run(() => _joinSync(h, req));
  }

  /// Kept for compatibility; prefer the specific isolate helpers above.
  /// NOTE: never pass closures capturing `this`/`DynamicLibrary` here.
  @Deprecated('Use the built-in async methods (devices/send/...) instead')
  Future<T> runBlocking<T>(FutureOr<T> Function() body) => Isolate.run(body);

  Future<List<dynamic>> devices() async {
    final h = _h.address;
    final j = await Isolate.run(() => _devicesSync(h));
    return (j['devices'] as List?) ?? const [];
  }

  Future<Map<String, dynamic>> send({
    required String peer,
    required List<String> paths,
    int? session,
  }) {
    final h = _h.address;
    final req = jsonEncode({
      'peer': peer,
      'paths': paths,
      'session': session,
    });
    return Isolate.run(() => _sendSync(h, req));
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
    final h = _h.address;
    final j = await Isolate.run(() => _syncListSync(h));
    final list = (j['folders'] as List?) ?? const [];
    return list.cast<Map<String, dynamic>>();
  }

  /// One event, waiting up to [timeoutMs]. Returns null on timeout.
  /// Call only from the isolate that owns the poll loop, or from the main
  /// isolate when no event stream is active (concurrent db_event calls
  /// serialize in Rust and add latency).
  Map<String, dynamic>? pollEventSync({int timeoutMs = 1000}) {
    final p = _event(_h, timeoutMs);
    if (p == nullptr) return null; // timeout (or error — check _lastErr)
    final s = p.toDartString();
    _free(p);
    return jsonDecode(s) as Map<String, dynamic>;
  }

  /// Event stream backed by a worker isolate long-polling db_event.
  /// The isolate is killed when the stream subscription is cancelled.
  Stream<Map<String, dynamic>> events() {
    late final StreamController<Map<String, dynamic>> controller;
    Isolate? worker;
    RawReceivePort? recv;

    controller = StreamController<Map<String, dynamic>>(
      onCancel: () async {
        worker?.kill(priority: Isolate.immediate);
        worker = null;
        recv?.close();
      },
    );

    recv = RawReceivePort();
    final handle = _h.address;

    recv.handler = (msg) {
      if (msg == null) {
        if (!controller.isClosed) controller.close();
        recv?.close();
        return;
      }
      if (msg is Map && !controller.isClosed) {
        controller.add(Map<String, dynamic>.from(msg));
      }
    };

    // Top-level entry + sendable args only (SendPort + int).
    Isolate.spawn(_eventsEntry, [recv.sendPort, handle]).then((iso) {
      worker = iso;
      if (controller.isClosed) iso.kill(priority: Isolate.immediate);
    }).catchError((Object e) {
      if (!controller.isClosed) controller.addError(e);
    });

    return controller.stream;
  }

  void shutdown() {
    if (_h != nullptr) {
      _shutdown(_h);
      _h = nullptr;
    }
  }
}
