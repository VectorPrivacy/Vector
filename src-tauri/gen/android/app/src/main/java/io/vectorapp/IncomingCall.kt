package io.vectorapp

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.graphics.BitmapFactory
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.graphics.drawable.IconCompat

/**
 * A call ringing while Vector isn't on screen: full screen over the lock screen where the
 * system allows it, a heads-up with Answer and Decline elsewhere, and a missed call once it
 * stops unanswered. Rust decides when; this only shows it.
 */
object IncomingCall {
    private const val RING_CHANNEL = "vector_incoming_calls"
    private const val MISSED_CHANNEL = "vector_missed_calls"
    private const val RING_ID = 3
    /** A little past the caller's own ring, so a lost withdrawal can't ring forever. */
    private const val RING_MS = 65_000L

    const val EXTRA_CALL_ID = "call_id"
    const val EXTRA_CALL_NPUB = "call_npub"
    const val EXTRA_CALL_ANSWER = "call_answer"
    const val ACTION_DECLINE = "io.vectorapp.ACTION_DECLINE_CALL"

    private data class Ringing(val callId: String, val npub: String, val name: String)

    @Volatile
    private var ringing: Ringing? = null

    init {
        System.loadLibrary("vector_lib")
    }

    @JvmStatic
    external fun nativeDecline(callId: String, npub: String)

    @JvmStatic
    fun ring(context: Context, callId: String, npub: String, name: String, avatarPath: String, video: Boolean) {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        channels(manager)
        ringing = Ringing(callId, npub, name)
        val show = activity(context, callId, npub, false, 20)
        val answer = activity(context, callId, npub, true, 21)
        val decline = PendingIntent.getBroadcast(
            context, 22,
            Intent(context, NotificationActionReceiver::class.java)
                .setAction(ACTION_DECLINE)
                .putExtra("chat_id", npub)
                .putExtra(EXTRA_CALL_ID, callId),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val person = Person.Builder().setName(name).setImportant(true).apply {
            avatar(avatarPath)?.let { setIcon(it) }
        }.build()
        val builder = NotificationCompat.Builder(context, RING_CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(name)
            .setContentText(if (video) "Incoming video call" else "Incoming call")
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setOngoing(true)
            .setAutoCancel(false)
            .setTimeoutAfter(RING_MS)
            .setContentIntent(show)
            .setFullScreenIntent(show, true)
        // The call style needs a full-screen intent the system will honour; without one, the
        // same two buttons as plain actions.
        val fullScreen = Build.VERSION.SDK_INT < 34 || manager.canUseFullScreenIntent()
        if (fullScreen) {
            builder.setStyle(NotificationCompat.CallStyle.forIncomingCall(person, decline, answer))
        } else {
            builder.addAction(0, "Decline", decline).addAction(0, "Answer", answer)
        }
        val notification = builder.build()
        // Rings until answered, declined or withdrawn, as a phone does.
        notification.flags = notification.flags or Notification.FLAG_INSISTENT
        try {
            manager.notify(RING_ID, notification)
        } catch (e: Exception) {
            android.util.Log.w("IncomingCall", "ring refused: ${e.message}")
        }
    }

    /** The ring stopped: taken up, or missed (timed out, or withdrawn by the caller). */
    @JvmStatic
    fun over(context: Context, callId: String, missed: Boolean) {
        val r = ringing ?: return
        if (r.callId != callId) return
        ringing = null
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        manager.cancel(RING_ID)
        if (!missed) return
        channels(manager)
        val open = PendingIntent.getActivity(
            context, ("missed:" + r.npub).hashCode(),
            Intent(context, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
                .putExtra("chat_id", r.npub),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val missedCall = NotificationCompat.Builder(context, MISSED_CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(r.name)
            .setContentText("Missed call")
            .setCategory(NotificationCompat.CATEGORY_MISSED_CALL)
            .setAutoCancel(true)
            .setContentIntent(open)
            .build()
        try {
            manager.notify(("missed:" + r.npub).hashCode(), missedCall)
        } catch (e: Exception) {
            android.util.Log.w("IncomingCall", "missed call refused: ${e.message}")
        }
    }

    /** A call that rang here only as a push, missed by its caller's word. */
    @JvmStatic
    fun missed(context: Context, callId: String, npub: String, name: String) {
        if (ringing?.callId != callId) ringing = Ringing(callId, npub, name)
        over(context, callId, true)
    }

    /** Declined from the notification. */
    @JvmStatic
    fun decline(context: Context, callId: String, npub: String) {
        if (ringing?.callId == callId) ringing = null
        context.getSystemService(NotificationManager::class.java)?.cancel(RING_ID)
        nativeDecline(callId, npub)
    }

    private fun activity(context: Context, callId: String, npub: String, answer: Boolean, request: Int): PendingIntent =
        PendingIntent.getActivity(
            context, request,
            Intent(context, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
                .putExtra(EXTRA_CALL_ID, callId)
                .putExtra(EXTRA_CALL_NPUB, npub)
                .putExtra(EXTRA_CALL_ANSWER, answer),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

    fun avatar(path: String): IconCompat? {
        if (path.isEmpty()) return null
        return try {
            BitmapFactory.decodeFile(path)?.let { IconCompat.createWithBitmap(it) }
        } catch (_: Exception) {
            null
        }
    }

    private fun channels(manager: NotificationManager) {
        if (manager.getNotificationChannel(RING_CHANNEL) == null) {
            val ring = NotificationChannel(RING_CHANNEL, "Incoming calls", NotificationManager.IMPORTANCE_HIGH).apply {
                description = "Rings when someone calls you"
                setSound(
                    RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE),
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                        .build()
                )
                enableVibration(true)
                vibrationPattern = longArrayOf(0, 800, 600, 800, 600)
                lockscreenVisibility = Notification.VISIBILITY_PUBLIC
            }
            manager.createNotificationChannel(ring)
        }
        if (manager.getNotificationChannel(MISSED_CHANNEL) == null) {
            val missed = NotificationChannel(MISSED_CHANNEL, "Missed calls", NotificationManager.IMPORTANCE_DEFAULT).apply {
                description = "Calls you didn't answer"
            }
            manager.createNotificationChannel(missed)
        }
    }
}
