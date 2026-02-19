package io.vectorapp

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Clears message history for a chat when its notification is swiped away. */
class NotificationDismissReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val historyKey = intent.getStringExtra("history_key") ?: return
        VectorNotificationService.clearMessageHistory(historyKey)
    }
}
