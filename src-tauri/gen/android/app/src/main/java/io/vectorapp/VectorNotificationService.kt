package io.vectorapp

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.work.Constraints
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import java.util.concurrent.TimeUnit
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

        fun schedulePeriodicPolling(context: Context) {
            val constraints = Constraints.Builder()
                .setRequiredNetworkType(NetworkType.CONNECTED)
                .build()

            // Immediate one-shot poll so we don't wait 15 minutes for the first check
            val immediateRequest = OneTimeWorkRequestBuilder<RelayPollWorker>()
                .setConstraints(constraints)
                .build()
            WorkManager.getInstance(context).enqueue(immediateRequest)

            // Periodic polling every 15 minutes as ongoing fallback
            val pollRequest = PeriodicWorkRequestBuilder<RelayPollWorker>(
                15, TimeUnit.MINUTES
            )
                .setConstraints(constraints)
                .build()

            WorkManager.getInstance(context).enqueueUniquePeriodicWork(
                "vector_relay_poll",
                ExistingPeriodicWorkPolicy.KEEP,
                pollRequest
            )
        }

        fun cancelPeriodicPolling(context: Context) {
            WorkManager.getInstance(context).cancelUniqueWork("vector_relay_poll")
        }

        /**
         * Post a message notification. Called from Rust JNI via the app's class loader.
         * Must be static and use applicationContext to avoid class loader issues
         * when called from JNI-attached threads.
         */
        @JvmStatic
        fun showMessageNotification(context: android.content.Context, title: String, body: String) {
            val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            val notificationId = notificationCounter.getAndIncrement()

            val launchIntent = Intent(context, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
            }
            val pendingIntent = PendingIntent.getActivity(
                context, notificationId, launchIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )

            val notification = NotificationCompat.Builder(context, MESSAGES_CHANNEL_ID)
                .setContentTitle(title)
                .setContentText(body)
                .setSmallIcon(R.mipmap.ic_launcher)
                .setAutoCancel(true)
                .setPriority(NotificationCompat.PRIORITY_HIGH)
                .setContentIntent(pendingIntent)
                .build()

            manager.notify(notificationId, notification)
            android.util.Log.d("VectorNotificationService", "Posted notification: $title")
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

        // Always schedule WorkManager polling as a safety net.
        // If started from MainActivity (full app), it will be cancelled immediately after.
        // If started by Android restart (service-only, no Tauri), this provides polling.
        schedulePeriodicPolling(this)
        android.util.Log.d("VectorNotificationService", "WorkManager polling scheduled")

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

        // Schedule WorkManager periodic polling as fallback
        schedulePeriodicPolling(this)
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
            .setSmallIcon(R.mipmap.ic_launcher)
            .setOngoing(true)
            .setContentIntent(pendingIntent)
            .build()
    }
}
