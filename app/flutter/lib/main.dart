// DropBridge 2026 Mobile Shell (Material 3).
//
// Features:
//  * Hardware Keystore (TEE/StrongBox) key integration
//  * Live interactive transfer progress cards with speed & ETA
//  * Foreground notification sync (real-time progress in Android shade)
//  * Target device selector BottomSheet on Share intent or file pick
//  * Haptic feedback engine on completion
//  * QR pairing view & joining
import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';
import 'package:permission_handler/permission_handler.dart';

import 'ffi_bridge.dart';

const _shareChannel = MethodChannel('dropbridge/share');

/// Request the runtime permissions DropBridge needs (Android 13+).
/// NEARBY_WIFI_DEVICES → LAN discovery/QUIC; POST_NOTIFICATIONS →
/// foreground transfer progress. Best-effort: never blocks startup.
Future<void> _requestRuntimePermissions() async {
  try {
    await [
      Permission.nearbyWifiDevices,
      Permission.notification,
      Permission.photos,
      Permission.videos,
      Permission.audio,
    ].request();
  } catch (_) {
    // permission_handler is a no-op on non-Android hosts (desktop tests).
  }
}

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await _requestRuntimePermissions();
  final appDir = await getApplicationSupportDirectory();
  final core = DropBridgeCore.load();

  // Retrieve hardware-backed seed from Android Keystore if available
  List<int>? hardwareKey;
  try {
    final keyBytes = await _shareChannel.invokeMethod<Uint8List>('getHardwareKey');
    if (keyBytes != null && keyBytes.length == 32) {
      hardwareKey = keyBytes.toList();
    }
  } catch (_) {
    // Fall back to software file protector on non-Android hosts or testing
  }

  await core.init(
    stateDir: '${appDir.path}/state',
    receiveDir: '${appDir.path}/received',
    name: 'Смартфон',
    kind: 'phone',
    hardwareKeySeed: hardwareKey,
  );
  runApp(DropBridgeApp(core: core));
}

class DropBridgeApp extends StatelessWidget {
  const DropBridgeApp({super.key, required this.core});
  final DropBridgeCore core;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'DropBridge',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        colorSchemeSeed: const Color(0xFF2563EB),
        useMaterial3: true,
        brightness: Brightness.light,
      ),
      darkTheme: ThemeData(
        colorSchemeSeed: const Color(0xFF3B82F6),
        useMaterial3: true,
        brightness: Brightness.dark,
      ),
      themeMode: ThemeMode.system,
      home: HomeScreen(core: core),
    );
  }
}

class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key, required this.core});
  final DropBridgeCore core;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  List<dynamic> _devices = const [];
  final _log = <String>[];
  StreamSubscription? _sub;
  String _status = 'Запуск…';

  // Active transfer state
  bool _isTransferring = false;
  String _activePeer = '';
  double _progressFraction = 0.0;
  String _transferSpeed = '';
  String _bytesProgressText = '';
  int _transferredBytes = 0;
  DateTime? _lastProgressUpdate;
  int _lastBytesSnapshot = 0;
  bool _receiveEnabled = true;
  int? _currentTransferSession;

  @override
  void initState() {
    super.initState();
    _refresh();
    _listenToEvents();

    _shareChannel.setMethodCallHandler((call) async {
      switch (call.method) {
        case 'share':
          final args = Map<String, dynamic>.from(call.arguments as Map);
          final paths = List<String>.from(args['paths'] as List);
          _note('Выбрано для отправки: ${paths.length} объ.');
          if (mounted) {
            await _handleOutgoingShare(paths);
          }
          break;
        case 'onCancelTransfer':
          _note('Передача отменена из уведомления');
          if (_currentTransferSession != null) {
            await widget.core.cancel(_currentTransferSession!);
            _currentTransferSession = null;
          }
          _shareChannel.invokeMethod('stopForeground');
          _lastBytesSnapshot = 0;
          _lastProgressUpdate = null;
          if (mounted) {
            setState(() {
              _isTransferring = false;
              _transferSpeed = '';
            });
          }
          break;
        case 'receiveMode':
          final args = Map<String, dynamic>.from(call.arguments as Map);
          final enabled = args['enabled'] == true;
          _note(enabled ? 'Плитка: приём включён' : 'Плитка: приём отключён');
          // Reflect the mode in the status line; the engine keeps running so
          // outgoing sends still work while auto-accept is suspended.
          setState(() {
            _receiveEnabled = enabled;
            if (!enabled && !_isTransferring) {
              _status = 'Приём отключён (плитка в шторке)';
            }
          });
          break;
      }
      return null;
    });
  }

  void _listenToEvents() {
    _sub = widget.core.events().listen((ev) {
      final kind = ev.keys.firstWhere((k) => k != 'v', orElse: () => 'event');
      final data = ev[kind] is Map ? Map<String, dynamic>.from(ev[kind] as Map) : null;

      if (kind == 'TransferProgress' && data != null) {
        if (data['session'] != null) {
          _currentTransferSession = (data['session'] as num?)?.toInt();
        }
        final delta = (data['bytes_delta'] as num?)?.toInt() ?? 0;
        final total = (data['total_bytes'] as num?)?.toInt() ?? 1;
        _onTransferProgress(delta, total);
      } else if (kind == 'TransferCompleted') {
        _currentTransferSession = null;
        if (data != null && data['ok'] == false) {
          _onTransferFailed(data);
        } else {
          _onTransferCompleted(data);
        }
      } else if (kind == 'TransferFailed') {
        _currentTransferSession = null;
        _onTransferFailed(data);
      } else if (kind == 'IncomingOffer' && data != null) {
        if (data['session'] != null) {
          _currentTransferSession = (data['session'] as num?)?.toInt();
        }
        final peer = data['peer_name'] ?? 'Устройство';
        final files = data['files'] ?? 1;
        final total = (data['total_bytes'] as num?)?.toInt() ?? 0;
        _startReceivingUi(peer.toString(), files as int, total);
      } else if (kind == 'TrustChanged') {
        _refresh();
      }

      _note('$kind: ${ev[kind] ?? ''}'.trim());
    });
  }

  void _onTransferProgress(int delta, int total) {
    if (!mounted) return;
    setState(() {
      _isTransferring = true;
      _transferredBytes += delta;
      _progressFraction = (_transferredBytes / total).clamp(0.0, 1.0);

      final now = DateTime.now();
      if (_lastProgressUpdate != null) {
        final ms = now.difference(_lastProgressUpdate!).inMilliseconds;
        if (ms >= 500) {
          final bytesDiff = _transferredBytes - _lastBytesSnapshot;
          final mbps = (bytesDiff / (ms / 1000)) / (1024 * 1024);
          _transferSpeed = '${mbps.toStringAsFixed(1)} МБ/с';
          _lastProgressUpdate = now;
          _lastBytesSnapshot = _transferredBytes;

          // Update Android notification shade in background
          final pct = (_progressFraction * 100).toInt();
          _shareChannel.invokeMethod('updateForeground', {
            'title': 'DropBridge: $_activePeer',
            'text': '$_transferSpeed • ${_formatBytes(_transferredBytes)} / ${_formatBytes(total)}',
            'percent': pct,
          });
        }
      } else {
        _lastProgressUpdate = now;
        _lastBytesSnapshot = _transferredBytes;
      }

      _bytesProgressText = '${_formatBytes(_transferredBytes)} / ${_formatBytes(total)}';
    });
  }

  void _onTransferCompleted(Map<String, dynamic>? data) {
    if (!mounted) return;
    setState(() {
      _isTransferring = false;
      _progressFraction = 1.0;
      _transferSpeed = '';
      _lastBytesSnapshot = 0;
      _lastProgressUpdate = null;
    });
    final files = (data?['files'] as List?)?.length ?? 1;
    _shareChannel.invokeMethod('completeForeground', {
      'title': 'Передача завершена',
      'text': 'Успешно передано файлов: $files',
    });
    _note('✓ Передача успешно завершена');
    _refresh();
  }

  void _onTransferFailed(Map<String, dynamic>? data) {
    if (!mounted) return;
    setState(() {
      _isTransferring = false;
      _transferSpeed = '';
      _lastBytesSnapshot = 0;
      _lastProgressUpdate = null;
    });
    _shareChannel.invokeMethod('stopForeground');
    final reason = data?['detail'] ?? data?['reason'] ?? 'Связь прервана';
    _note('✗ Ошибка передачи: $reason');
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text('Ошибка передачи: $reason'),
          backgroundColor: Theme.of(context).colorScheme.error,
          duration: const Duration(seconds: 5),
        ),
      );
    }
  }

  void _startReceivingUi(String peer, int files, int total) {
    setState(() {
      _isTransferring = true;
      _activePeer = peer;
      _transferredBytes = 0;
      _progressFraction = 0.0;
      _bytesProgressText = '0 Б / ${_formatBytes(total)}';
    });
    _shareChannel.invokeMethod('startForeground', {
      'title': 'Приём от $peer',
      'text': 'Файлов: $files • ${_formatBytes(total)}',
    });
  }

  Future<void> _refresh() async {
    try {
      final d = await widget.core.devices();
      setState(() {
        _devices = d;
        _status = !_receiveEnabled
            ? 'Приём отключён (плитка в шторке)'
            : (d.isEmpty ? 'Нет сопряжённых устройств' : 'Сопряжённых устройств: ${d.length}');
      });
    } catch (e) {
      setState(() => _status = 'Ошибка: $e');
    }
  }

  void _note(String s) => setState(() {
        _log.insert(0, s);
        if (_log.length > 200) _log.removeLast();
      });

  Future<void> _handleOutgoingShare(List<String> paths) async {
    if (_devices.isEmpty) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('Нет сопряжённых устройств. Сначала выполните сопряжение с компьютером.')),
      );
      return;
    }
    if (_devices.length == 1) {
      final target = _devices.first as Map;
      await _executeSend(target['name'] as String, paths);
    } else {
      // Multiple devices: open bottom sheet for quick target selection
      final selected = await _showDevicePickerBottomSheet(paths);
      if (selected != null) {
        await _executeSend(selected, paths);
      }
    }
  }

  Future<String?> _showDevicePickerBottomSheet(List<String> paths) async {
    return showModalBottomSheet<String>(
      context: context,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(24)),
      ),
      builder: (ctx) {
        return SafeArea(
          child: Padding(
            padding: const EdgeInsets.symmetric(vertical: 16),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 8),
                  child: Row(
                    children: [
                      const Icon(Icons.send_to_mobile, size: 24),
                      const SizedBox(width: 12),
                      Expanded(
                        child: Text(
                          'Отправить ${paths.length} файл(ов) на:',
                          style: Theme.of(ctx).textTheme.titleMedium?.copyWith(
                                fontWeight: FontWeight.bold,
                              ),
                        ),
                      ),
                    ],
                  ),
                ),
                const Divider(),
                ..._devices.map((d) {
                  final dev = d as Map;
                  final name = dev['name'] as String;
                  final kind = (dev['kind'] as String?)?.toLowerCase() ?? 'laptop';
                  return ListTile(
                    leading: CircleAvatar(
                      backgroundColor: Theme.of(ctx).colorScheme.primaryContainer,
                      child: Icon(_deviceIcon(kind)),
                    ),
                    title: Text(name, style: const TextStyle(fontWeight: FontWeight.w600)),
                    subtitle: Text(_deviceKindLabel(kind)),
                    trailing: const Icon(Icons.arrow_forward_ios, size: 16),
                    onTap: () => Navigator.pop(ctx, name),
                  );
                }),
              ],
            ),
          ),
        );
      },
    );
  }

  Future<void> _executeSend(String peerName, List<String> paths) async {
    setState(() {
      _isTransferring = true;
      _activePeer = peerName;
      _transferredBytes = 0;
      _progressFraction = 0.0;
      _bytesProgressText = 'Подготовка к передаче…';
    });
    _shareChannel.invokeMethod('startForeground', {
      'title': 'Отправка на $peerName',
      'text': 'Выбрано файлов: ${paths.length}',
    });

    try {
      final r = await widget.core.send(peer: peerName, paths: paths);
      if (r['ok'] == true) {
        final b = (r['bytes'] as num?)?.toInt() ?? 0;
        _note('✓ Доставлено ${_formatBytes(b)} на $peerName');
        _shareChannel.invokeMethod('vibrate');
      } else {
        _note('✗ Ошибка отправки: ${r['detail']}');
        if (mounted) {
          ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(
              content: Text('Ошибка отправки: ${r['detail']}'),
              backgroundColor: Theme.of(context).colorScheme.error,
            ),
          );
        }
      }
    } catch (e) {
      _note('Ошибка отправки: $e');
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text('Ошибка отправки: $e'),
            backgroundColor: Theme.of(context).colorScheme.error,
          ),
        );
      }
    } finally {
      setState(() => _isTransferring = false);
      _shareChannel.invokeMethod('stopForeground');
    }
  }

  Future<void> _pickAndSend() async {
    final paths = await _shareChannel.invokeMethod<List<dynamic>>('pick');
    if (paths == null || paths.isEmpty) return;
    await _handleOutgoingShare(paths.cast<String>());
  }

  Future<void> _joinByPaste() async {
    final text = await showDialog<String>(
      context: context,
      builder: (_) => const _PasteQrDialog(),
    );
    if (text == null || text.trim().isEmpty) return;
    try {
      await widget.core.join(text.trim());
      _note('✓ Сопряжение выполнено успешно');
      _shareChannel.invokeMethod('vibrate');
      _refresh();
    } catch (e) {
      _note('Ошибка сопряжения: $e');
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(
            content: Text('Ошибка сопряжения: $e'),
            backgroundColor: Theme.of(context).colorScheme.error,
          ),
        );
      }
    }
  }

  Future<void> _showPairQr() async {
    final qr = await widget.core.pairQr();
    if (!mounted) return;
    showDialog(
      context: context,
      builder: (_) => AlertDialog(
        title: const Text('Сопряжение с компьютером'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Container(
              padding: const EdgeInsets.all(16),
              decoration: BoxDecoration(
                color: Colors.white,
                borderRadius: BorderRadius.circular(16),
                border: Border.all(color: Colors.grey.shade300),
              ),
              child: SelectableText(
                qr['qr'] as String,
                style: const TextStyle(fontSize: 10, color: Colors.black87),
              ),
            ),
            const SizedBox(height: 12),
            const Text(
              'Отсканируйте этот QR-код в DropBridge на ПК с Windows 11 или скопируйте строку ниже.',
              style: TextStyle(fontSize: 12, color: Colors.grey),
              textAlign: TextAlign.center,
            ),
          ],
        ),
        actions: [
          TextButton.icon(
            onPressed: () {
              Clipboard.setData(ClipboardData(text: qr['qr'] as String));
              Navigator.pop(context);
              ScaffoldMessenger.of(context).showSnackBar(
                const SnackBar(content: Text('Код сопряжения скопирован в буфер обмена')),
              );
            },
            icon: const Icon(Icons.copy, size: 18),
            label: const Text('Скопировать код'),
          ),
          TextButton(onPressed: () => Navigator.pop(context), child: const Text('Закрыть')),
        ],
      ),
    );
  }

  IconData _deviceIcon(String kind) {
    switch (kind.toLowerCase()) {
      case 'desktop':
        return Icons.desktop_windows;
      case 'tablet':
        return Icons.tablet_android;
      case 'phone':
        return Icons.phone_android;
      default:
        return Icons.laptop;
    }
  }

  String _deviceKindLabel(String kind) {
    switch (kind.toLowerCase()) {
      case 'desktop':
        return 'Компьютер (ПК)';
      case 'laptop':
        return 'Ноутбук';
      case 'tablet':
        return 'Планшет';
      case 'phone':
        return 'Смартфон';
      default:
        return 'Устройство ($kind)';
    }
  }

  String _formatBytes(int bytes) {
    if (bytes < 1024) return '$bytes Б';
    if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(1)} КБ';
    if (bytes < 1024 * 1024 * 1024) return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} МБ';
    return '${(bytes / (1024 * 1024 * 1024)).toStringAsFixed(2)} ГБ';
  }

  @override
  void dispose() {
    _sub?.cancel();
    widget.core.shutdown();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        title: const Text('DropBridge', style: TextStyle(fontWeight: FontWeight.bold)),
        actions: [
          IconButton(
            tooltip: 'Ввести ключ сопряжения',
            onPressed: _joinByPaste,
            icon: const Icon(Icons.qr_code_scanner),
          ),
          IconButton(
            tooltip: 'Показать код сопряжения',
            onPressed: _showPairQr,
            icon: const Icon(Icons.qr_code_2),
          ),
        ],
      ),
      body: Column(
        children: [
          // Live Transfer Progress Card
          if (_isTransferring)
            Padding(
              padding: const EdgeInsets.all(16),
              child: Card(
                elevation: 4,
                color: theme.colorScheme.primaryContainer,
                shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(20)),
                child: Padding(
                  padding: const EdgeInsets.all(16),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Row(
                        children: [
                          const Icon(Icons.sync, size: 20),
                          const SizedBox(width: 8),
                          Expanded(
                            child: Text(
                              'Передача: $_activePeer',
                              style: theme.textTheme.titleSmall?.copyWith(
                                fontWeight: FontWeight.bold,
                              ),
                            ),
                          ),
                          if (_transferSpeed.isNotEmpty)
                            Text(
                              _transferSpeed,
                              style: theme.textTheme.bodySmall?.copyWith(
                                fontWeight: FontWeight.bold,
                                color: theme.colorScheme.primary,
                              ),
                            ),
                        ],
                      ),
                      const SizedBox(height: 12),
                      LinearProgressIndicator(
                        value: _progressFraction > 0 ? _progressFraction : null,
                        borderRadius: BorderRadius.circular(8),
                        minHeight: 8,
                      ),
                      const SizedBox(height: 8),
                      Row(
                        mainAxisAlignment: MainAxisAlignment.spaceBetween,
                        children: [
                          Text(
                            _bytesProgressText,
                            style: theme.textTheme.bodySmall,
                          ),
                          Row(
                            children: [
                              Text(
                                '${(_progressFraction * 100).toInt()}%',
                                style: theme.textTheme.bodySmall?.copyWith(fontWeight: FontWeight.bold),
                              ),
                              const SizedBox(width: 12),
                              InkWell(
                                onTap: () async {
                                  if (_currentTransferSession != null) {
                                    await widget.core.cancel(_currentTransferSession!);
                                    _currentTransferSession = null;
                                  }
                                  _shareChannel.invokeMethod('stopForeground');
                                  _lastBytesSnapshot = 0;
                                  _lastProgressUpdate = null;
                                  if (mounted) {
                                    setState(() {
                                      _isTransferring = false;
                                      _transferSpeed = '';
                                    });
                                  }
                                  _note('Передача отменена пользователем');
                                },
                                borderRadius: BorderRadius.circular(12),
                                child: Padding(
                                  padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
                                  child: Row(
                                    children: [
                                      Icon(Icons.close, size: 16, color: theme.colorScheme.error),
                                      const SizedBox(width: 4),
                                      Text(
                                        'Отмена',
                                        style: theme.textTheme.bodySmall?.copyWith(
                                          color: theme.colorScheme.error,
                                          fontWeight: FontWeight.bold,
                                        ),
                                      ),
                                    ],
                                  ),
                                ),
                              ),
                            ],
                          ),
                        ],
                      ),
                    ],
                  ),
                ),
              ),
            ),

          // Status & Device Header
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
            child: Row(
              children: [
                Icon(Icons.devices, size: 18, color: theme.colorScheme.primary),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    _status,
                    style: theme.textTheme.bodyMedium?.copyWith(fontWeight: FontWeight.w500),
                  ),
                ),
                TextButton.icon(
                  onPressed: _refresh,
                  icon: const Icon(Icons.refresh, size: 16),
                  label: const Text('Обновить'),
                ),
              ],
            ),
          ),

          // Paired Devices List
          Expanded(
            child: _devices.isEmpty
                ? Center(
                    child: Column(
                      mainAxisAlignment: MainAxisAlignment.center,
                      children: [
                        Icon(Icons.computer, size: 64, color: Colors.grey.shade400),
                        const SizedBox(height: 16),
                        const Text(
                          'Нет сопряжённых компьютеров.',
                          style: TextStyle(fontSize: 16, fontWeight: FontWeight.w600),
                        ),
                        const SizedBox(height: 8),
                        const Text(
                          'Нажмите на значок QR вверху для сопряжения с ПК с Windows 11.',
                          style: TextStyle(color: Colors.grey),
                          textAlign: TextAlign.center,
                        ),
                      ],
                    ),
                  )
                : ListView.builder(
                    padding: const EdgeInsets.symmetric(horizontal: 12),
                    itemCount: _devices.length,
                    itemBuilder: (_, i) {
                      final d = _devices[i] as Map;
                      final name = d['name'] as String;
                      final kind = (d['kind'] as String?) ?? 'laptop';
                      return Card(
                        margin: const EdgeInsets.symmetric(vertical: 6),
                        elevation: 1,
                        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(16)),
                        child: ListTile(
                          leading: CircleAvatar(
                            backgroundColor: theme.colorScheme.secondaryContainer,
                            child: Icon(_deviceIcon(kind)),
                          ),
                          title: Text(name, style: const TextStyle(fontWeight: FontWeight.w600)),
                          subtitle: Text(_deviceKindLabel(kind)),
                          trailing: FilledButton.tonalIcon(
                            onPressed: _pickAndSend,
                            icon: const Icon(Icons.upload, size: 16),
                            label: const Text('Отправить'),
                          ),
                          onTap: _pickAndSend,
                        ),
                      );
                    },
                  ),
          ),

          // Console / Diagnostics Log Box
          Container(
            height: 120,
            color: theme.colorScheme.surfaceContainerHighest.withValues(alpha: 0.5),
            child: ListView(
              reverse: true,
              padding: const EdgeInsets.all(8),
              children: _log.map((l) => Text(l, style: const TextStyle(fontSize: 11, fontFamily: 'monospace'))).toList(),
            ),
          ),
        ],
      ),
      floatingActionButton: FloatingActionButton.extended(
        onPressed: _pickAndSend,
        icon: const Icon(Icons.add),
        label: const Text('Отправить файлы'),
      ),
    );
  }
}

class _PasteQrDialog extends StatefulWidget {
  const _PasteQrDialog();
  @override
  State<_PasteQrDialog> createState() => _PasteQrDialogState();
}

class _PasteQrDialogState extends State<_PasteQrDialog> {
  final _ctrl = TextEditingController();
  @override
  Widget build(BuildContext context) => AlertDialog(
        title: const Text('Вставить ключ сопряжения'),
        content: TextField(
          controller: _ctrl,
          maxLines: 4,
          decoration: const InputDecoration(
            hintText: 'dropbridge://pair?...',
            border: OutlineInputBorder(),
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Отмена'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, _ctrl.text),
            child: const Text('Подключить'),
          ),
        ],
      );
}
