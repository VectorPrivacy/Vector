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

/**
 * Keeps the process, and so the client's relay connections, alive while the app is away, and
 * posts its message notifications. The Tauri client's channels and ids, so an update in place
 * keeps the user's channel settings.
 */
class VectorNotificationService : Service() {

    companion object {
        const val SERVICE_CHANNEL_ID = "vector_service"
        const val MESSAGES_CHANNEL_ID = "vector_messages_v2"
        const val SERVICE_NOTIFICATION_ID = 1

        // One row for every notification without a chat, so a replay can't pile them up.
        private const val GENERIC_NOTIFICATION_ID = 0x56454331

        /** MessagingStyle shows only what each post replays, so each chat keeps its recent lines. */
        private data class ChatMessage(val senderName: String, val senderAvatarPath: String, val body: String, val timestamp: Long)
        private val chatMessageHistory = java.util.concurrent.ConcurrentHashMap<String, MutableList<ChatMessage>>()

        /** Called from Rust on any thread, so it reaches everything through `context`. */
        @JvmStatic
        fun showMessageNotification(
            context: Context,
            title: String,
            body: String,
            avatarPath: String,
            chatId: String,
            senderName: String,
            groupName: String,
            groupAvatarPath: String,
        ) {
            Strings.init(context)
            val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            val isGroup = groupName.isNotEmpty()
            val historyKey = chatId.ifEmpty { title }

            // Per chat and deterministic, so a cancel needs no state that a racing post could clear.
            val notificationId = if (chatId.isNotEmpty()) chatId.hashCode() else GENERIC_NOTIFICATION_ID
            // Stable request codes: a PendingIntent outlives the process, and a reused one keeps
            // its old extras unless UPDATE_CURRENT replaces them.
            val pendingRequestCode = notificationId * 2 + 1073741823

            val launchIntent = Intent(context, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_SINGLE_TOP
                if (chatId.isNotEmpty()) putExtra("chat_id", chatId)
            }
            val pendingIntent = PendingIntent.getActivity(
                context, pendingRequestCode, launchIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            val deleteIntent = Intent(context, NotificationDismissReceiver::class.java).apply {
                putExtra("history_key", historyKey)
            }
            val deletePendingIntent = PendingIntent.getBroadcast(
                context, pendingRequestCode, deleteIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )

            val history = synchronized(chatMessageHistory) {
                val h = chatMessageHistory.getOrPut(historyKey) { mutableListOf() }
                h.add(ChatMessage(
                    if (isGroup) senderName.ifEmpty { Strings.t("android-notify-someone", "Someone") } else senderName.ifEmpty { title },
                    avatarPath, body, System.currentTimeMillis()
                ))
                while (h.size > 8) h.removeAt(0)
                h.toList()
            }

            val messagingStyle = NotificationCompat.MessagingStyle(Person.Builder().setName(Strings.t("android-notify-me", "Me")).build())
            if (isGroup) {
                messagingStyle.setConversationTitle(groupName)
                messagingStyle.setGroupConversation(true)
            }
            for (msg in history) {
                val person = Person.Builder()
                    .setName(msg.senderName)
                    .apply { loadBitmap(msg.senderAvatarPath)?.let { setIcon(IconCompat.createWithBitmap(it)) } }
                    .build()
                messagingStyle.addMessage(msg.body, msg.timestamp, person)
            }

            val largeIcon = (if (isGroup) loadBitmap(groupAvatarPath) ?: loadBitmap(avatarPath) else loadBitmap(avatarPath))
                ?: BitmapFactory.decodeResource(context.resources, R.drawable.ic_large_icon)

            val builder = NotificationCompat.Builder(context, MESSAGES_CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_notification)
                .setLargeIcon(largeIcon)
                .setStyle(messagingStyle)
                .setAutoCancel(true)
                .setPriority(NotificationCompat.PRIORITY_HIGH)
                .setContentIntent(pendingIntent)
                .setDeleteIntent(deletePendingIntent)
                .setOnlyAlertOnce(chatId.isEmpty())

            // A notification without a chat (a locked account's) has nothing to act on.
            if (chatId.isNotEmpty()) {
                val markReadIntent = Intent(context, NotificationActionReceiver::class.java).apply {
                    action = NotificationActionReceiver.ACTION_MARK_READ
                    putExtra("chat_id", chatId)
                    putExtra("notification_id", notificationId)
                }
                val markReadPending = PendingIntent.getBroadcast(
                    context, notificationId * 2, markReadIntent,
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
                )
                val remoteInput = androidx.core.app.RemoteInput.Builder(NotificationActionReceiver.REPLY_KEY)
                    .setLabel(Strings.t("android-notify-reply", "Reply"))
                    .build()
                val replyIntent = Intent(context, NotificationActionReceiver::class.java).apply {
                    action = NotificationActionReceiver.ACTION_REPLY
                    putExtra("chat_id", chatId)
                    putExtra("notification_id", notificationId)
                }
                val replyPending = PendingIntent.getBroadcast(
                    context, notificationId * 2 + 1, replyIntent,
                    PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
                )
                val replyAction = NotificationCompat.Action.Builder(R.drawable.ic_notification, Strings.t("android-notify-reply", "Reply"), replyPending)
                    .addRemoteInput(remoteInput)
                    .build()
                builder.addAction(R.drawable.ic_notification, Strings.t("android-notify-mark-read", "Mark Read"), markReadPending)
                builder.addAction(replyAction)
            }

            manager.notify(notificationId, builder.build())
        }

        /** The user is back in the app: a later notification starts its conversation afresh. */
        @JvmStatic
        fun clearAllMessageHistory() {
            chatMessageHistory.clear()
        }

        @JvmStatic
        fun clearMessageHistory(historyKey: String) {
            chatMessageHistory.remove(historyKey)
        }

        @JvmStatic
        fun cancelNotification(context: Context, chatId: String) {
            if (chatId.isEmpty()) return
            chatMessageHistory.remove(chatId)
            val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            manager.cancel(chatId.hashCode())
        }

        /** Every message notification and its history; the service's own row stays. */
        @JvmStatic
        fun cancelAllMessageNotifications(context: Context) {
            chatMessageHistory.clear()
            val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            for (shown in manager.activeNotifications) {
                if (shown.notification.channelId == MESSAGES_CHANNEL_ID) manager.cancel(shown.tag, shown.id)
            }
        }

        /** Both channels, before anything posts to them. */
        @JvmStatic
        fun createChannels(context: Context) {
            Strings.init(context)
            val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            manager.createNotificationChannel(
                NotificationChannel(SERVICE_CHANNEL_ID, Strings.t("android-channel-service", "Background Service"), NotificationManager.IMPORTANCE_MIN).apply {
                    description = Strings.t("android-channel-service-about", "Keeps Vector connected for real-time messages")
                    setShowBadge(false)
                }
            )
            // The first channel had the system sound, which a channel can never change.
            manager.deleteNotificationChannel("vector_messages")
            val prelude = Uri.parse("android.resource://${context.packageName}/raw/notif_prelude")
            val audio = AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_NOTIFICATION)
                .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                .build()
            manager.createNotificationChannel(
                NotificationChannel(MESSAGES_CHANNEL_ID, Strings.t("android-channel-messages", "Messages"), NotificationManager.IMPORTANCE_HIGH).apply {
                    description = Strings.t("android-channel-messages-about", "New message notifications")
                    setSound(prelude, audio)
                    enableVibration(true)
                }
            )
        }

        private fun loadBitmap(path: String): Bitmap? {
            if (path.isEmpty()) return null
            return try { BitmapFactory.decodeFile(path) } catch (e: Exception) { null }
        }
    }

    private val serviceTagline: String by lazy {
        listOf(
            Strings.t("android-service-tagline-watch", "Keeping watch"),
            Strings.t("android-service-tagline-miss", "You won't miss a thing"),
            Strings.t("android-service-tagline-standby", "Standing by"),
        ).random()
    }

    override fun onCreate() {
        super.onCreate()
        createChannels(this)
    }

    // Not sticky: restarted without the app, there would be no client behind the row.
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForeground(SERVICE_NOTIFICATION_ID, buildServiceNotification())
        return START_NOT_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun buildServiceNotification(): Notification {
        val launchIntent = Intent(this, MainActivity::class.java).apply { flags = Intent.FLAG_ACTIVITY_SINGLE_TOP }
        val pendingIntent = PendingIntent.getActivity(
            this, 0, launchIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val body = android.text.SpannableStringBuilder()
            .append(Strings.t("android-service-listening", "Listening for messages"))
            .append(" · ")
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
