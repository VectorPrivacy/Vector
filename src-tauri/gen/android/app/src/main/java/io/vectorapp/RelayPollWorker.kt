package io.vectorapp

import android.app.NotificationManager
import android.content.Context
import androidx.core.app.NotificationCompat
import androidx.work.CoroutineWorker
import androidx.work.WorkerParameters

class RelayPollWorker(
    context: Context,
    params: WorkerParameters
) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result {
        android.util.Log.d("RelayPollWorker", "doWork() starting, attempt #$runAttemptCount")
        return try {
            val dataDir = applicationContext.dataDir.absolutePath
            android.util.Log.d("RelayPollWorker", "Calling pollForNewMessages with dataDir=$dataDir")

            val startTime = System.currentTimeMillis()
            val result = pollForNewMessages(dataDir)
            val elapsed = System.currentTimeMillis() - startTime

            android.util.Log.d("RelayPollWorker", "pollForNewMessages returned in ${elapsed}ms, result length=${result?.length ?: -1}, value='${result?.take(200)}'")

            if (result != null && result.isNotEmpty() && result != "-1") {
                val notifications = result.split("\n").filter { it.contains("|") }
                for (notification in notifications) {
                    val parts = notification.split("|", limit = 2)
                    if (parts.size == 2) {
                        showNotification(parts[0], parts[1])
                    }
                }
                android.util.Log.d("RelayPollWorker", "Posted ${notifications.size} notifications")
            } else if (result == "-1") {
                android.util.Log.w("RelayPollWorker", "Poll returned error (-1)")
            } else {
                android.util.Log.d("RelayPollWorker", "No new messages (empty result)")
            }
            Result.success()
        } catch (e: Exception) {
            android.util.Log.e("RelayPollWorker", "Poll failed with exception: ${e.javaClass.name}: ${e.message}", e)
            Result.retry()
        } catch (e: Error) {
            android.util.Log.e("RelayPollWorker", "Poll failed with error: ${e.javaClass.name}: ${e.message}", e)
            Result.failure()
        }
    }

    private fun showNotification(title: String, body: String) {
        val manager = applicationContext.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

        // Generate a unique notification ID from the title
        val notificationId = title.hashCode() and 0x7FFFFFFF

        val notification = NotificationCompat.Builder(applicationContext, VectorNotificationService.MESSAGES_CHANNEL_ID)
            .setContentTitle(title)
            .setContentText(body)
            .setSmallIcon(R.mipmap.ic_launcher)
            .setAutoCancel(true)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .build()

        manager.notify(notificationId, notification)
    }

    // JNI method implemented in Rust (background_sync.rs)
    // Returns newline-separated "title|body" notification data, or empty string
    // filesDir: path to app's internal files directory for database access
    private external fun pollForNewMessages(filesDir: String): String

    companion object {
        init {
            System.loadLibrary("vector_lib")
        }
    }
}
