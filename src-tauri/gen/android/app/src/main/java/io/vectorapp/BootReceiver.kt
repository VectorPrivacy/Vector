package io.vectorapp

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build

class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == Intent.ACTION_BOOT_COMPLETED) {
            if (VectorBatteryHelper.getBackgroundServiceEnabled(context)) {
                android.util.Log.d("BootReceiver", "Boot completed, starting foreground service")
                val serviceIntent = Intent(context, VectorNotificationService::class.java)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    context.startForegroundService(serviceIntent)
                } else {
                    context.startService(serviceIntent)
                }
            } else {
                android.util.Log.d("BootReceiver", "Boot completed, background service disabled by user")
            }
        }
    }
}
