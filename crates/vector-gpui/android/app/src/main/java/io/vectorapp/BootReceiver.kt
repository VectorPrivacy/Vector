package io.vectorapp

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Back in the background after a restart, when the user keeps Vector listening. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        if (VectorBatteryHelper.getBackgroundServiceEnabled(context)) {
            context.startForegroundService(Intent(context, VectorNotificationService::class.java))
        }
    }
}
