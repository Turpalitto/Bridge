package app.dropbridge.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.net.Uri
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File

/**
 * 2026 Modern Android Main Activity for DropBridge.
 *
 * Interfaces with:
 *  - SAF document picker
 *  - Native Foreground Service for live transfer notifications
 *  - Hardware Android Keystore for TEE/StrongBox identity
 *  - Haptic feedback engine
 *  - Share target intake
 */
class MainActivity : FlutterActivity() {

    private lateinit var channel: MethodChannel
    private var pickCallback: MethodChannel.Result? = null

    private val cancelReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            if (intent?.action == TransferForegroundService.ACTION_CANCEL_TRANSFER) {
                channel.invokeMethod("onCancelTransfer", null)
            }
        }
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQ_PICK) {
            val staged = ArrayList<String>()
            data?.clipData?.let { clip ->
                for (i in 0 until clip.itemCount) {
                    stageUri(clip.getItemAt(i).uri)?.let(staged::add)
                }
            } ?: data?.data?.let { uri ->
                stageUri(uri)?.let(staged::add)
            }
            pickCallback?.success(staged)
            pickCallback = null
        }
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        channel = MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "dropbridge/share")

        channel.setMethodCallHandler { call, result ->
            when (call.method) {
                "pick" -> {
                    pickCallback = result
                    val pickIntent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                        addCategory(Intent.CATEGORY_OPENABLE)
                        type = "*/*"
                        putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                    }
                    startActivityForResult(pickIntent, REQ_PICK)
                }
                "getHardwareKey" -> {
                    val protector = AndroidKeyStoreProtector(this)
                    val seed = protector.getOrCreateIdentitySeed()
                    result.success(seed)
                }
                "startForeground" -> {
                    val title = call.argument<String>("title") ?: "DropBridge"
                    val text = call.argument<String>("text") ?: "Подготовка к передаче…"
                    val intent = Intent(this, TransferForegroundService::class.java).apply {
                        action = TransferForegroundService.ACTION_START
                        putExtra(TransferForegroundService.EXTRA_TITLE, title)
                        putExtra(TransferForegroundService.EXTRA_TEXT, text)
                    }
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                        startForegroundService(intent)
                    } else {
                        startService(intent)
                    }
                    result.success(null)
                }
                "updateForeground" -> {
                    val title = call.argument<String>("title") ?: "Передача файлов…"
                    val text = call.argument<String>("text") ?: ""
                    val percent = call.argument<Int>("percent") ?: 0
                    val intent = Intent(this, TransferForegroundService::class.java).apply {
                        action = TransferForegroundService.ACTION_UPDATE
                        putExtra(TransferForegroundService.EXTRA_TITLE, title)
                        putExtra(TransferForegroundService.EXTRA_TEXT, text)
                        putExtra(TransferForegroundService.EXTRA_PERCENT, percent)
                    }
                    startService(intent)
                    result.success(null)
                }
                "completeForeground" -> {
                    val title = call.argument<String>("title") ?: "Передача завершена"
                    val text = call.argument<String>("text") ?: "Все файлы успешно переданы."
                    val intent = Intent(this, TransferForegroundService::class.java).apply {
                        action = TransferForegroundService.ACTION_COMPLETE
                        putExtra(TransferForegroundService.EXTRA_TITLE, title)
                        putExtra(TransferForegroundService.EXTRA_TEXT, text)
                    }
                    startService(intent)
                    vibrateSuccess()
                    result.success(null)
                }
                "stopForeground" -> {
                    val intent = Intent(this, TransferForegroundService::class.java).apply {
                        action = TransferForegroundService.ACTION_STOP
                    }
                    startService(intent)
                    result.success(null)
                }
                "vibrate" -> {
                    vibrateSuccess()
                    result.success(null)
                }
                else -> result.notImplemented()
            }
        }

        // Register cancel receiver from notification action
        val filter = IntentFilter(TransferForegroundService.ACTION_CANCEL_TRANSFER)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            registerReceiver(cancelReceiver, filter, RECEIVER_NOT_EXPORTED)
        } else {
            registerReceiver(cancelReceiver, filter)
        }

        // Deliver staged share paths (from ShareEntryActivity) to Dart.
        deliverShareIntent(intent)
        deliverTileState(intent)
    }

    override fun onDestroy() {
        super.onDestroy()
        try {
            unregisterReceiver(cancelReceiver)
        } catch (_: Exception) {}
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        deliverShareIntent(intent)
        deliverTileState(intent)
    }

    /** Forward the Quick Settings tile receive-mode to Dart. */
    private fun deliverTileState(intent: Intent?) {
        val enabled = intent?.getBooleanExtra(EXTRA_TILE_TOGGLED, false) ?: return
        channel.invokeMethod("receiveMode", mapOf("enabled" to enabled))
        intent.removeExtra(EXTRA_TILE_TOGGLED)
    }

    private fun deliverShareIntent(intent: Intent?) {
        val paths = intent?.getStringArrayListExtra(EXTRA_STAGED_PATHS) ?: return
        if (paths.isEmpty()) return
        channel.invokeMethod("share", mapOf("paths" to paths))
        intent.removeExtra(EXTRA_STAGED_PATHS)
    }

    private fun vibrateSuccess() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val vm = getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as? VibratorManager
            vm?.defaultVibrator?.vibrate(VibrationEffect.createPredefined(VibrationEffect.EFFECT_CLICK))
        } else {
            @Suppress("DEPRECATION")
            val v = getSystemService(Context.VIBRATOR_SERVICE) as? Vibrator
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                v?.vibrate(VibrationEffect.createOneShot(50, VibrationEffect.DEFAULT_AMPLITUDE))
            } else {
                @Suppress("DEPRECATION")
                v?.vibrate(50)
            }
        }
    }

    /** Copy a content:// URI into the app staging dir; returns the file path. */
    private fun stageUri(uri: Uri): String? = runCatching {
        val name = queryDisplayName(uri) ?: "shared-${System.nanoTime()}"
        val dir = File(filesDir, "staged").apply { mkdirs() }
        val out = File(dir, uniqueName(dir, name))
        contentResolver.openInputStream(uri)?.use { input ->
            out.outputStream().use { input.copyTo(it) }
        } ?: return@runCatching null
        out.absolutePath
    }.getOrNull()

    private fun queryDisplayName(uri: Uri): String? = runCatching {
        contentResolver.query(uri, null, null, null, null)?.use { c ->
            val idx = c.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME)
            if (idx >= 0 && c.moveToFirst()) c.getString(idx) else null
        }
    }.getOrNull()

    private fun uniqueName(dir: File, name: String): String {
        var candidate = name
        var i = 1
        while (File(dir, candidate).exists()) {
            val dot = name.lastIndexOf('.')
            candidate = if (dot > 0) "${name.substring(0, dot)}-$i${name.substring(dot)}" else "$name-$i"
            i++
        }
        return candidate
    }

    companion object {
        private const val REQ_PICK = 1001
        const val EXTRA_STAGED_PATHS = "dropbridge.staged_paths"
        const val EXTRA_TILE_TOGGLED = "dropbridge.tile_toggled"
    }
}
