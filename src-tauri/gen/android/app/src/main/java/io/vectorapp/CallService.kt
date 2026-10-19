package io.vectorapp

import android.Manifest
import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.view.WindowManager
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat

/**
 * Runs for as long as a call does. Android silences a backgrounded app's microphone, and
 * soon its playback, unless a foreground service of the right type holds the process.
 */
class CallService : Service() {

    companion object {
        private const val CHANNEL_ID = "vector_calls"
        private const val NOTIFICATION_ID = 2
        private const val ACTION_LEAVE = "io.vectorapp.ACTION_LEAVE_CALL"

        init {
            System.loadLibrary("vector_lib")
        }

        @JvmStatic
        external fun nativeLeaveCall()

        @JvmStatic
        fun start(context: Context) {
            try {
                ContextCompat.startForegroundService(context, Intent(context, CallService::class.java))
            } catch (e: Exception) {
                // Refused from the background (Android 12+): the call goes on, unheld.
                android.util.Log.w("CallService", "start refused: ${e.message}")
            }
        }

        @JvmStatic
        fun stop(context: Context) {
            context.stopService(Intent(context, CallService::class.java))
        }

        /** Video on screen: the display stays lit while the call shows it. */
        @JvmStatic
        fun keepScreenOn(activity: Activity, on: Boolean) {
            activity.runOnUiThread {
                if (on) {
                    activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                } else {
                    activity.window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                }
            }
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_LEAVE) {
            nativeLeaveCall()
            return START_NOT_STICKY
        }
        val manager = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(CHANNEL_ID, "Calls", NotificationManager.IMPORTANCE_LOW).apply {
                description = "Shown while you're in a call"
                setShowBadge(false)
            }
            manager.createNotificationChannel(channel)
        }
        val open = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val leave = PendingIntent.getService(
            this, 1,
            Intent(this, CallService::class.java).setAction(ACTION_LEAVE),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle("In a call")
            .setContentText("Tap to return to Vector")
            .setContentIntent(open)
            .addAction(0, "Leave", leave)
            .setOngoing(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .build()
        // Listen-only (no microphone permission) can't claim the microphone type.
        val mic = checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
        val type = when {
            Build.VERSION.SDK_INT < Build.VERSION_CODES.Q -> 0
            mic && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R ->
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE or ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
            else -> ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
        }
        try {
            ServiceCompat.startForeground(this, NOTIFICATION_ID, notification, type)
        } catch (e: Exception) {
            android.util.Log.w("CallService", "startForeground refused: ${e.message}")
            stopSelf()
        }
        return START_NOT_STICKY
    }

    /** Swiped away from recents: the call ends with the app, as quitting does on desktop. */
    override fun onTaskRemoved(rootIntent: Intent?) {
        nativeLeaveCall()
        stopSelf()
    }
}
