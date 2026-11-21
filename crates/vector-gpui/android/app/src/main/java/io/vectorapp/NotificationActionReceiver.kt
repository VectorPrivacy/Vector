package io.vectorapp

import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.RemoteInput

/** The shade's buttons: decline a ringing call, or mark a chat read or reply to it. */
class NotificationActionReceiver : BroadcastReceiver() {

    companion object {
        const val ACTION_MARK_READ = "io.vectorapp.ACTION_MARK_READ"
        const val ACTION_REPLY = "io.vectorapp.ACTION_REPLY"
        const val REPLY_KEY = "key_reply_text"

        init {
            System.loadLibrary("vector_gpui")
        }

        @JvmStatic
        external fun nativeMarkAsRead(chatId: String)
        @JvmStatic
        external fun nativeSendReply(chatId: String, content: String)
    }

    override fun onReceive(context: Context, intent: Intent) {
        val chatId = intent.getStringExtra("chat_id") ?: return
        when (intent.action) {
            IncomingCall.ACTION_DECLINE -> {
                val callId = intent.getStringExtra(IncomingCall.EXTRA_CALL_ID) ?: return
                IncomingCall.decline(context, callId, chatId)
            }
            ACTION_MARK_READ -> {
                nativeMarkAsRead(chatId)
                val id = intent.getIntExtra("notification_id", -1)
                if (id != -1) context.getSystemService(NotificationManager::class.java)?.cancel(id)
                VectorNotificationService.clearMessageHistory(chatId)
            }
            ACTION_REPLY -> {
                val reply = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(REPLY_KEY)?.toString() ?: return
                // The reply is posted back under the conversation, which clears the shade's spinner.
                nativeSendReply(chatId, reply)
            }
        }
    }
}
