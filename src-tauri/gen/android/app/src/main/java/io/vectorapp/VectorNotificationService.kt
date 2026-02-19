package io.vectorapp

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.graphics.BitmapFactory
import android.os.IBinder
import androidx.core.app.NotificationCompat
import java.util.concurrent.atomic.AtomicInteger

class VectorNotificationService : Service() {

    companion object {
        const val SERVICE_CHANNEL_ID = "vector_service"
        const val MESSAGES_CHANNEL_ID = "vector_messages"
        const val SERVICE_NOTIFICATION_ID = 1

        /** Incrementing counter for unique message notification IDs (enables stacking). */
        private val notificationCounter = AtomicInteger(100)

        init {
            System.loadLibrary("vector_lib")
        }

        @JvmStatic
        external fun nativeStartBackgroundSync(dataDir: String, context: android.content.Context)
        @JvmStatic
        external fun nativeStopBackgroundSync()

        /**
         * Post a message notification. Called from Rust JNI via the app's class loader.
         * Must be static and use applicationContext to avoid class loader issues
         * when called from JNI-attached threads.
         */
        @JvmStatic
        fun showMessageNotification(context: android.content.Context, title: String, body: String, avatarPath: String) {
            val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            val notificationId = notificationCounter.getAndIncrement()

            val launchIntent = Intent(context, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
            }
            val pendingIntent = PendingIntent.getActivity(
                context, notificationId, launchIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )

            // Use sender's cached avatar if available, otherwise fall back to app icon
            val largeIcon = if (avatarPath.isNotEmpty()) {
                try {
                    BitmapFactory.decodeFile(avatarPath)
                } catch (e: Exception) {
                    android.util.Log.w("VectorNotificationService", "Failed to load avatar: ${e.message}")
                    null
                }
            } else null
            val finalLargeIcon = largeIcon ?: BitmapFactory.decodeResource(context.resources, R.drawable.ic_large_icon)

            val notification = NotificationCompat.Builder(context, MESSAGES_CHANNEL_ID)
                .setContentTitle(title)
                .setContentText(body)
                .setSmallIcon(R.drawable.ic_notification)
                .setLargeIcon(finalLargeIcon)
                .setAutoCancel(true)
                .setPriority(NotificationCompat.PRIORITY_HIGH)
                .setContentIntent(pendingIntent)
                .build()

            manager.notify(notificationId, notification)
            android.util.Log.d("VectorNotificationService", "Posted notification: $title (avatar: ${avatarPath.isNotEmpty()})")
        }
    }

    override fun onCreate() {
        super.onCreate()
        createNotificationChannels()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        android.util.Log.d("VectorNotificationService", "onStartCommand called")
        val notification = buildServiceNotification()
        startForeground(SERVICE_NOTIFICATION_ID, notification)
        android.util.Log.d("VectorNotificationService", "startForeground done")

        // Signal Rust that background sync mode is active.
        // Pass dataDir so standalone mode can find the npub account database.
        val dataDir = applicationContext.dataDir.absolutePath
        android.util.Log.d("VectorNotificationService", "Calling nativeStartBackgroundSync with dataDir=$dataDir")
        try {
            nativeStartBackgroundSync(dataDir, applicationContext)
            android.util.Log.d("VectorNotificationService", "nativeStartBackgroundSync returned OK")
        } catch (e: Exception) {
            android.util.Log.e("VectorNotificationService", "Failed to start background sync (Exception): ${e.message}", e)
        } catch (e: Error) {
            android.util.Log.e("VectorNotificationService", "Failed to start background sync (Error): ${e.javaClass.name}: ${e.message}", e)
        }

        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        super.onDestroy()

        // Signal Rust to stop background sync
        try {
            nativeStopBackgroundSync()
        } catch (e: Exception) {
            android.util.Log.e("VectorNotificationService", "Failed to stop background sync: ${e.message}")
        }
    }

    override fun onTaskRemoved(rootIntent: Intent?) {
        super.onTaskRemoved(rootIntent)
        android.util.Log.d("VectorNotificationService", "Task removed (app swiped), starting sync immediately")
        // The service process is still alive after the swipe (foreground service keeps it).
        // Start background sync directly — no need to wait for AlarmManager or START_STICKY.
        try {
            startForeground(SERVICE_NOTIFICATION_ID, buildServiceNotification())
            val dataDir = applicationContext.dataDir.absolutePath
            nativeStartBackgroundSync(dataDir, applicationContext)
            android.util.Log.d("VectorNotificationService", "Sync started from onTaskRemoved")
        } catch (e: Exception) {
            android.util.Log.e("VectorNotificationService", "Failed to start sync from onTaskRemoved: ${e.message}")
            // Fallback: schedule alarm restart in case direct start failed
            val restartIntent = Intent(applicationContext, VectorNotificationService::class.java)
            val pendingIntent = PendingIntent.getService(
                applicationContext, 1, restartIntent,
                PendingIntent.FLAG_ONE_SHOT or PendingIntent.FLAG_IMMUTABLE
            )
            val alarmManager = getSystemService(Context.ALARM_SERVICE) as android.app.AlarmManager
            alarmManager.set(
                android.app.AlarmManager.ELAPSED_REALTIME_WAKEUP,
                android.os.SystemClock.elapsedRealtime() + 1000,
                pendingIntent
            )
        }
    }

    private fun createNotificationChannels() {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

        // Low-priority persistent "running" channel
        val serviceChannel = NotificationChannel(
            SERVICE_CHANNEL_ID,
            "Background Service",
            NotificationManager.IMPORTANCE_LOW
        ).apply {
            description = "Keeps Vector connected for real-time messages"
            setShowBadge(false)
        }
        manager.createNotificationChannel(serviceChannel)

        // High-priority message notification channel
        val messagesChannel = NotificationChannel(
            MESSAGES_CHANNEL_ID,
            "Messages",
            NotificationManager.IMPORTANCE_HIGH
        ).apply {
            description = "New message notifications"
            enableVibration(true)
        }
        manager.createNotificationChannel(messagesChannel)
    }

    private fun buildServiceNotification(): Notification {
        val launchIntent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
        }
        val pendingIntent = PendingIntent.getActivity(
            this, 0, launchIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        return NotificationCompat.Builder(this, SERVICE_CHANNEL_ID)
            .setContentTitle("Vector")
            .setContentText("Connected for messages")
            .setSmallIcon(R.drawable.ic_notification)
            .setOngoing(true)
            .setContentIntent(pendingIntent)
            .build()
    }
}
