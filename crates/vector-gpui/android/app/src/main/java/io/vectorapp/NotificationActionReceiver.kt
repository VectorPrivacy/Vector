package io.vectorapp

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** The shade's buttons. Mark Read and Reply arrive with message notifications. */
class NotificationActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val chatId = intent.getStringExtra("chat_id") ?: return
        when (intent.action) {
            IncomingCall.ACTION_DECLINE -> {
                val callId = intent.getStringExtra(IncomingCall.EXTRA_CALL_ID) ?: return
                IncomingCall.decline(context, callId, chatId)
            }
        }
    }
}
