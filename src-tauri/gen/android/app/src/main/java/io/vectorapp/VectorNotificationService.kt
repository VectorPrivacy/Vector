package io.vectorapp

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.AudioAttributes
import android.net.Uri
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.graphics.drawable.IconCompat
import java.util.concurrent.atomic.AtomicInteger

class VectorNotificationService : Service() {

    companion object {
        const val SERVICE_CHANNEL_ID = "vector_service"
        const val MESSAGES_CHANNEL_ID = "vector_messages_v2"
        const val SERVICE_NOTIFICATION_ID = 1

        /** Incrementing counter for unique notification IDs (DMs and non-chat notifications). */
        private val notificationCounter = AtomicInteger(100)

        /** Stable notification ID per chat — lets MessagingStyle accumulate messages in one notification. */
        private val chatNotificationIds = java.util.concurrent.ConcurrentHashMap<String, Int>()

        /** Message history per chat — MessagingStyle requires replaying all messages on each update. */
        private data class ChatMessage(val senderName: String, val senderAvatarPath: String, val body: String, val timestamp: Long)
        private val chatMessageHistory = java.util.concurrent.ConcurrentHashMap<String, MutableList<ChatMessage>>()

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
         *
         * For group messages (groupName non-empty), uses MessagingStyle with:
         * - Group avatar as the large icon
         * - Sender avatar inline with their name
         * - Conversation title = group name
         *
         * For DMs (groupName empty), uses a simple notification with sender avatar.
         */
        @JvmStatic
        fun showMessageNotification(
            context: android.content.Context,
            title: String,
            body: String,
            avatarPath: String,
            chatId: String,
            senderName: String,
            groupName: String,
            groupAvatarPath: String,
        ) {
            val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            val isGroup = groupName.isNotEmpty()
            val historyKey = chatId.ifEmpty { title }

            // Stable notification ID per chat so messages stack in one entry
            val notificationId = if (chatId.isNotEmpty()) {
                chatNotificationIds.getOrPut(chatId) { notificationCounter.getAndIncrement() }
            } else {
                notificationCounter.getAndIncrement()
            }

            // Fresh requestCode per post — avoids PendingIntent caching issues with FLAG_IMMUTABLE
            val pendingRequestCode = notificationCounter.getAndIncrement()

            val launchIntent = Intent(context, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
                if (chatId.isNotEmpty()) {
                    putExtra("chat_id", chatId)
                }
            }
            val pendingIntent = PendingIntent.getActivity(
                context, pendingRequestCode, launchIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )

            // DeleteIntent to clear message history when notification is swiped away
            val deleteIntent = Intent(context, NotificationDismissReceiver::class.java).apply {
                putExtra("history_key", historyKey)
            }
            val deletePendingIntent = PendingIntent.getBroadcast(
                context, pendingRequestCode, deleteIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )

            // Accumulate message history for this chat (synchronized to prevent
            // corruption if two notifications arrive simultaneously for the same chat)
            val history = synchronized(chatMessageHistory) {
                val h = chatMessageHistory.getOrPut(historyKey) { mutableListOf() }
                h.add(ChatMessage(
                    if (isGroup) senderName.ifEmpty { "Someone" } else senderName.ifEmpty { title },
                    avatarPath, body, System.currentTimeMillis()
                ))
                while (h.size > 8) h.removeAt(0)
                h.toList() // snapshot under lock
            }

            // Build MessagingStyle — used for both groups and DMs
            val messagingStyle = NotificationCompat.MessagingStyle(
                Person.Builder().setName("Me").build()
            )
            if (isGroup) {
                messagingStyle.setConversationTitle(groupName)
                messagingStyle.setGroupConversation(true)
            }

            for (msg in history) {
                val icon = loadBitmapIcon(msg.senderAvatarPath)
                val person = Person.Builder()
                    .setName(msg.senderName)
                    .apply { icon?.let { setIcon(it) } }
                    .build()
                messagingStyle.addMessage(msg.body, msg.timestamp, person)
            }

            val largeIcon = if (isGroup) {
                loadBitmap(groupAvatarPath) ?: loadBitmap(avatarPath)
            } else {
                loadBitmap(avatarPath)
            } ?: BitmapFactory.decodeResource(context.resources, R.drawable.ic_large_icon)

            val builder = NotificationCompat.Builder(context, MESSAGES_CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_notification)
                .setLargeIcon(largeIcon)
                .setStyle(messagingStyle)
                .setAutoCancel(true)
                .setPriority(NotificationCompat.PRIORITY_HIGH)
                .setContentIntent(pendingIntent)
                .setDeleteIntent(deletePendingIntent)

            // Only add Mark Read / Reply actions when we have a valid chatId.
            // Encrypted account notifications pass empty chatId (can't decrypt),
            // so these actions would be non-functional.
            if (chatId.isNotEmpty()) {
                val markReadIntent = Intent(context, NotificationActionReceiver::class.java).apply {
                    action = NotificationActionReceiver.ACTION_MARK_READ
                    putExtra("chat_id", chatId)
                    putExtra("notification_id", notificationId)
                }
                val markReadPending = PendingIntent.getBroadcast(
                    context, notificationCounter.getAndIncrement(), markReadIntent,
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
                )

                val remoteInput = androidx.core.app.RemoteInput.Builder(NotificationActionReceiver.REPLY_KEY)
                    .setLabel("Reply")
                    .build()
                val replyIntent = Intent(context, NotificationActionReceiver::class.java).apply {
                    action = NotificationActionReceiver.ACTION_REPLY
                    putExtra("chat_id", chatId)
                    putExtra("notification_id", notificationId)
                }
                val replyPending = PendingIntent.getBroadcast(
                    context, notificationCounter.getAndIncrement(), replyIntent,
                    PendingIntent.FLAG_MUTABLE
                )
                val replyAction = NotificationCompat.Action.Builder(
                    R.drawable.ic_notification, "Reply", replyPending
                ).addRemoteInput(remoteInput).build()

                builder.addAction(R.drawable.ic_notification, "Mark Read", markReadPending)
                builder.addAction(replyAction)
            }

            val notification = builder.build()

            manager.notify(notificationId, notification)

            android.util.Log.d("VectorNotificationService", "Posted notification #$notificationId: $title (group: $isGroup, chat: ${chatId.take(20)})")
        }

        /** Clear all message history (called when Activity resumes — notifications are no longer relevant). */
        @JvmStatic
        fun clearAllMessageHistory() {
            chatMessageHistory.clear()
            chatNotificationIds.clear()
        }

        /** Clear message history for a specific chat (called when a notification is dismissed). */
        @JvmStatic
        fun clearMessageHistory(historyKey: String) {
            chatMessageHistory.remove(historyKey)
            chatNotificationIds.remove(historyKey)
        }

        private fun loadBitmap(path: String): Bitmap? {
            if (path.isEmpty()) return null
            return try {
                BitmapFactory.decodeFile(path)
            } catch (e: Exception) {
                null
            }
        }

        private fun loadBitmapIcon(path: String): IconCompat? {
            return loadBitmap(path)?.let { IconCompat.createWithBitmap(it) }
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

        // Minimum-priority persistent "running" channel (hidden from status bar)
        val serviceChannel = NotificationChannel(
            SERVICE_CHANNEL_ID,
            "Background Service",
            NotificationManager.IMPORTANCE_MIN
        ).apply {
            description = "Keeps Vector connected for real-time messages"
            setShowBadge(false)
        }
        manager.createNotificationChannel(serviceChannel)

        // Delete the old channel (v1 used default system sound, immutable after creation).
        // The new v2 channel is created below with the Prelude custom sound.
        manager.deleteNotificationChannel("vector_messages")

        val preludeUri = Uri.parse("android.resource://${packageName}/raw/notif_prelude")
        val audioAttributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_NOTIFICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
            .build()

        val messagesChannel = NotificationChannel(
            MESSAGES_CHANNEL_ID,
            "Messages",
            NotificationManager.IMPORTANCE_HIGH
        ).apply {
            description = "New message notifications"
            setSound(preludeUri, audioAttributes)
            enableVibration(true)
        }
        manager.createNotificationChannel(messagesChannel)
    }

    /** Warm taglines — one is picked at random per service boot. */
    private val serviceTagline: String by lazy {
        listOf(
            "Keeping watch",
            "You won't miss a thing",
            "Standing by",
        ).random()
    }

    private fun buildServiceNotification(): Notification {
        val launchIntent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
        }
        val pendingIntent = PendingIntent.getActivity(
            this, 0, launchIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        val body = android.text.SpannableStringBuilder()
            .append("Listening for messages \u00B7 ")
            .append(serviceTagline, android.text.style.StyleSpan(android.graphics.Typeface.ITALIC), android.text.Spannable.SPAN_EXCLUSIVE_EXCLUSIVE)

        return NotificationCompat.Builder(this, SERVICE_CHANNEL_ID)
            .setContentTitle("Vector")
            .setContentText(body)
            .setSmallIcon(R.drawable.ic_notification)
            .setOngoing(true)
            .setContentIntent(pendingIntent)
            .build()
    }
}
