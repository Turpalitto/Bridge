package app.dropbridge.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.IBinder

/**
 * 2026 Modern Android Foreground Service for DropBridge file transfers.
 *
 * Displays rich interactive notifications with real-time percentage progress bars,
 * transfer speed (MB/s), file counters, and a quick cancel action.
 * Active ONLY during live data movement to guarantee zero background battery drain.
 */
class TransferForegroundService : Service() {

    private lateinit var notificationManager: NotificationManager

    override fun onCreate() {
        super.onCreate()
        notificationManager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        ensureChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> {
                val title = intent.getStringExtra(EXTRA_TITLE) ?: "Передача файлов DropBridge"
                val text = intent.getStringExtra(EXTRA_TEXT) ?: "Подготовка к передаче…"
                val notification = buildProgressNotification(title, text, 0, indeterminate = true)
                startForeground(NOTIF_ID, notification)
            }
            ACTION_UPDATE -> {
                val title = intent.getStringExtra(EXTRA_TITLE) ?: "Передача файлов…"
                val text = intent.getStringExtra(EXTRA_TEXT) ?: ""
                val percent = intent.getIntExtra(EXTRA_PERCENT, 0)
                val notification = buildProgressNotification(title, text, percent, indeterminate = false)
                notificationManager.notify(NOTIF_ID, notification)
            }
            ACTION_COMPLETE -> {
                val title = intent.getStringExtra(EXTRA_TITLE) ?: "Передача завершена"
                val text = intent.getStringExtra(EXTRA_TEXT) ?: "Все файлы успешно получены."
                val notification = buildCompletedNotification(title, text)
                stopForeground(STOP_FOREGROUND_REMOVE)
                notificationManager.notify(NOTIF_ID + 1, notification)
                stopSelf()
            }
            ACTION_STOP -> {
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
            }
        }
        return START_NOT_STICKY
    }

    private fun buildProgressNotification(
        title: String,
        text: String,
        percent: Int,
        indeterminate: Boolean
    ): Notification {
        val openAppIntent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
        }
        val openPending = PendingIntent.getActivity(
            this,
            0,
            openAppIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        val cancelIntent = Intent(ACTION_CANCEL_TRANSFER).apply {
            setPackage(packageName)
        }
        val cancelPending = PendingIntent.getBroadcast(
            this,
            1,
            cancelIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }

        builder.setContentTitle(title)
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_sys_upload)
            .setContentIntent(openPending)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setProgress(100, percent.coerceIn(0, 100), indeterminate)
            .addAction(
                Notification.Action.Builder(
                    null,
                    "Отмена",
                    cancelPending
                ).build()
            )

        return builder.build()
    }

    private fun buildCompletedNotification(title: String, text: String): Notification {
        val openAppIntent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
        }
        val openPending = PendingIntent.getActivity(
            this,
            0,
            openAppIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }

        return builder.setContentTitle(title)
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_sys_upload_done)
            .setContentIntent(openPending)
            .setAutoCancel(true)
            .build()
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            if (notificationManager.getNotificationChannel(CHANNEL) == null) {
                val channel = NotificationChannel(
                    CHANNEL,
                    "Передача файлов",
                    NotificationManager.IMPORTANCE_LOW
                ).apply {
                    description = "Ход и статус активной передачи файлов"
                    setShowBadge(false)
                }
                notificationManager.createNotificationChannel(channel)
            }
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        const val CHANNEL = "dropbridge_transfers"
        const val NOTIF_ID = 4101

        const val ACTION_START = "app.dropbridge.app.action.START"
        const val ACTION_UPDATE = "app.dropbridge.app.action.UPDATE"
        const val ACTION_COMPLETE = "app.dropbridge.app.action.COMPLETE"
        const val ACTION_STOP = "app.dropbridge.app.action.STOP"
        const val ACTION_CANCEL_TRANSFER = "app.dropbridge.app.action.CANCEL"

        const val EXTRA_TITLE = "extra_title"
        const val EXTRA_TEXT = "extra_text"
        const val EXTRA_PERCENT = "extra_percent"
    }
}
