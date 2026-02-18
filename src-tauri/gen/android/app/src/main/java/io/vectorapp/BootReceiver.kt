package io.vectorapp

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == Intent.ACTION_BOOT_COMPLETED) {
            android.util.Log.d("BootReceiver", "Boot completed, scheduling periodic polling")
            VectorNotificationService.schedulePeriodicPolling(context)
        }
    }
}
