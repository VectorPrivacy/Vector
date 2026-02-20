package io.vectorapp

import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.RemoteInput

/**
 * Handles notification action buttons:
 * - ACTION_MARK_READ: marks the chat as read via JNI and dismisses the notification.
 * - ACTION_REPLY: reads inline reply text, sends via JNI, and lets Rust re-post the
 *   notification with the reply appended (clears the RemoteInput spinner).
 */
class NotificationActionReceiver : BroadcastReceiver() {

    companion object {
        const val ACTION_MARK_READ = "io.vectorapp.ACTION_MARK_READ"
        const val ACTION_REPLY = "io.vectorapp.ACTION_REPLY"
        const val REPLY_KEY = "key_reply_text"

        init { System.loadLibrary("vector_lib") }

        @JvmStatic
        external fun nativeMarkAsRead(chatId: String)
        @JvmStatic
        external fun nativeSendReply(chatId: String, content: String)
    }

    override fun onReceive(context: Context, intent: Intent) {
        val chatId = intent.getStringExtra("chat_id") ?: return
        val notificationId = intent.getIntExtra("notification_id", -1)

        when (intent.action) {
            ACTION_MARK_READ -> {
                nativeMarkAsRead(chatId)
                // Dismiss the notification immediately
                if (notificationId != -1) {
                    val manager = context.getSystemService(NotificationManager::class.java)
                    manager?.cancel(notificationId)
                }
                VectorNotificationService.clearMessageHistory(chatId)
            }
            ACTION_REPLY -> {
                val reply = RemoteInput.getResultsFromIntent(intent)
                    ?.getCharSequence(REPLY_KEY)?.toString() ?: return
                nativeSendReply(chatId, reply)
                // Rust will re-post the notification with the reply appended,
                // which clears the RemoteInput spinner automatically.
            }
        }
    }
}
