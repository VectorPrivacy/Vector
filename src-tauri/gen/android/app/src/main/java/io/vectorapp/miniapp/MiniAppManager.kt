package io.vectorapp.miniapp

import android.app.Activity
import android.view.ViewGroup
import android.view.animation.AccelerateInterpolator
import android.view.animation.DecelerateInterpolator
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import io.vectorapp.Logger
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Singleton manager for Mini App WebView overlays.
 *
 * Manages the lifecycle of Mini App WebViews that overlay on top of the main
 * Tauri WebView. Only one Mini App can be open at a time.
 *
 * Methods annotated with @JvmStatic are called from Rust via JNI.
 */
object MiniAppManager {
    private const val TAG = "MiniAppManager"

    private var currentMiniApp: MiniAppInstance? = null
    private var rootView: ViewGroup? = null
    private var activity: Activity? = null

    /** Queue for realtime data — Rust pushes, JS polls via MiniAppIpc */
    private val realtimeQueue = ConcurrentLinkedQueue<String>()
    private val notifyPending = AtomicBoolean(false)

    data class MiniAppInstance(
        val webView: MiniAppWebView,
        val miniappId: String,
        val packagePath: String,
        val chatId: String,
        val messageId: String
    )

    init {
        System.loadLibrary("vector_lib")
    }

    /**
     * Initialize the manager with the activity.
     * Should be called from MainActivity.onWebViewCreate()
     */
    @JvmStatic
    fun initialize(activity: Activity) {
        this.activity = activity
        // Get the content view (FrameLayout that contains the main WebView)
        this.rootView = activity.findViewById(android.R.id.content)
        Logger.debug(TAG, "MiniAppManager initialized")
    }

    /**
     * Open a Mini App in a full-screen overlay WebView.
     * If another Mini App is already open, it will be closed first.
     */
    @JvmStatic
    fun openMiniApp(
        miniappId: String,
        packagePath: String,
        chatId: String,
        messageId: String,
        href: String?
    ) {
        val act = activity ?: run {
            Logger.error(TAG, "Cannot open Mini App: Activity not initialized", null)
            return
        }

        // Must run on UI thread
        act.runOnUiThread {
            Logger.info(TAG, "Opening Mini App: $miniappId (chat: $chatId, message: $messageId)")

            // Close any existing Mini App first
            closeMiniAppInternal(animate = false)

            // Enter immersive fullscreen mode
            enterImmersiveMode()

            // Create the Mini App WebView
            val webView = MiniAppWebView(act, miniappId, packagePath)

            // Add to root view with slide-in animation
            rootView?.let { root ->
                root.addView(webView)

                // Animate slide-in from bottom
                webView.translationY = root.height.toFloat()
                webView.animate()
                    .translationY(0f)
                    .setDuration(300)
                    .setInterpolator(DecelerateInterpolator())
                    .start()
            }

            // Build the URL
            val url = if (href != null && href.isNotEmpty()) {
                val cleanHref = if (href.startsWith("/")) href else "/$href"
                "http://webxdc.localhost$cleanHref"
            } else {
                "http://webxdc.localhost/"
            }

            Logger.debug(TAG, "Loading Mini App URL: $url")
            webView.loadUrl(url)

            // Store instance
            currentMiniApp = MiniAppInstance(
                webView = webView,
                miniappId = miniappId,
                packagePath = packagePath,
                chatId = chatId,
                messageId = messageId
            )

            // Notify Rust that Mini App opened
            try {
                onMiniAppOpened(miniappId, chatId, messageId)
            } catch (e: Exception) {
                Logger.error(TAG, "Failed to notify Rust of Mini App open: ${e.message}", null)
            }
        }
    }

    /**
     * Close the currently open Mini App.
     */
    @JvmStatic
    fun closeMiniApp() {
        activity?.runOnUiThread {
            closeMiniAppInternal(animate = true)
        }
    }

    private fun closeMiniAppInternal(animate: Boolean) {
        val instance = currentMiniApp ?: return

        Logger.info(TAG, "Closing Mini App: ${instance.miniappId}")

        // Exit immersive mode to restore system UI
        exitImmersiveMode()

        // Notify Rust first
        try {
            onMiniAppClosed(instance.miniappId)
        } catch (e: Exception) {
            Logger.error(TAG, "Failed to notify Rust of Mini App close: ${e.message}", null)
        }

        if (animate) {
            // Animate WebView slide out
            instance.webView.animate()
                .translationY(rootView?.height?.toFloat() ?: 0f)
                .setDuration(200)
                .setInterpolator(AccelerateInterpolator())
                .withEndAction {
                    rootView?.removeView(instance.webView)
                    instance.webView.destroy()
                }
                .start()
        } else {
            // Immediate removal (no animation)
            rootView?.removeView(instance.webView)
            instance.webView.destroy()
        }

        currentMiniApp = null
    }

    /**
     * Send an event to the currently open Mini App.
     */
    @JvmStatic
    fun sendToMiniApp(event: String, data: String) {
        val webView = currentMiniApp?.webView ?: return

        activity?.runOnUiThread {
            val script = """
                (function() {
                    try {
                        window.dispatchEvent(new CustomEvent('$event', { detail: $data }));
                    } catch(e) {
                        console.error('Failed to dispatch event:', e);
                    }
                })();
            """.trimIndent()

            webView.evaluateJavascript(script, null)
        }
    }

    /**
     * Send realtime data to the Mini App listener.
     * Uses a queue + notify pattern to avoid flooding the UI thread:
     * 1. Data is pushed to a ConcurrentLinkedQueue (lock-free, any thread)
     * 2. A tiny evaluateJavascript notification triggers JS to pull data
     * 3. JS calls __MINIAPP_IPC__.pollRealtimeData() on a background thread
     * This prevents 170KB+ base91 strings from being compiled as JS 20x/sec.
     */
    @JvmStatic
    fun sendRealtimeData(data: String) {
        val webView = currentMiniApp?.webView ?: return

        realtimeQueue.add(data)

        // Coalesce notifications: only post one evaluateJavascript if none pending.
        // compareAndSet is atomic — exactly one thread wins even under contention.
        // Losing threads' data is already in the queue and will be picked up by the poll.
        if (notifyPending.compareAndSet(false, true)) {
            activity?.runOnUiThread {
                notifyPending.set(false)
                webView.evaluateJavascript("if(window.__miniapp_rt_notify)window.__miniapp_rt_notify()", null)
            }
        }
    }

    /**
     * Poll all queued realtime data items.
     * Called from JS via MiniAppIpc on a background thread.
     * Returns a JSON array of base91 strings, or null if empty.
     */
    @JvmStatic
    fun pollRealtimeData(): String? {
        if (realtimeQueue.isEmpty()) return null

        val sb = StringBuilder()
        sb.append('[')
        var first = true
        while (true) {
            val item = realtimeQueue.poll() ?: break
            if (!first) sb.append(',')
            sb.append('"')
            sb.append(item) // base91 doesn't contain " or \ so no escaping needed
            sb.append('"')
            first = false
        }
        sb.append(']')
        return if (first) null else sb.toString() // null if nothing was polled
    }

    /**
     * Close the Mini App due to a renderer crash.
     * Notifies Rust so the frontend can show a toast.
     *
     * Ordering: onMiniAppCrashed() fires immediately on the calling thread,
     * then closeMiniAppInternal() runs later on the UI thread (which calls
     * onMiniAppClosed()). Rust receives crash before close — this is intentional:
     * the crash event triggers a toast, the close event does cleanup.
     */
    @JvmStatic
    fun closeMiniAppFromCrash() {
        val miniappId = currentMiniApp?.miniappId
        activity?.runOnUiThread {
            closeMiniAppInternal(animate = false)
        }
        if (miniappId != null) {
            try {
                onMiniAppCrashed(miniappId)
            } catch (e: Exception) {
                Logger.error(TAG, "Failed to notify Rust of Mini App crash: ${e.message}", null)
            }
        }
    }

    /**
     * Check if a Mini App is currently open.
     */
    @JvmStatic
    fun isOpen(): Boolean = currentMiniApp != null

    /**
     * Get the current Mini App ID, or null if none open.
     */
    @JvmStatic
    fun getCurrentMiniAppId(): String? = currentMiniApp?.miniappId

    /**
     * Get the package path for the current Mini App.
     */
    @JvmStatic
    fun getCurrentPackagePath(): String? = currentMiniApp?.packagePath

    /**
     * Forward permission results from Activity.onRequestPermissionsResult()
     */
    @JvmStatic
    fun handlePermissionResult(requestCode: Int, grantResults: IntArray) {
        currentMiniApp?.webView?.handlePermissionResult(requestCode, grantResults)
    }

    /**
     * Enter immersive fullscreen mode - hides status bar and navigation bar.
     * Uses sticky immersive mode so bars temporarily appear on edge swipe.
     */
    private fun enterImmersiveMode() {
        val act = activity ?: return

        try {
            // Use WindowInsetsControllerCompat for cross-version compatibility
            val windowInsetsController = WindowCompat.getInsetsController(act.window, act.window.decorView)

            // Hide both system bars
            windowInsetsController.hide(WindowInsetsCompat.Type.systemBars())

            // Use sticky immersive mode - bars appear temporarily on swipe then auto-hide
            windowInsetsController.systemBarsBehavior =
                WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE

            Logger.debug(TAG, "Entered immersive mode")
        } catch (e: Exception) {
            Logger.error(TAG, "Failed to enter immersive mode: ${e.message}", null)
        }
    }

    /**
     * Exit immersive mode - restores status bar and navigation bar.
     */
    private fun exitImmersiveMode() {
        val act = activity ?: return

        try {
            val windowInsetsController = WindowCompat.getInsetsController(act.window, act.window.decorView)

            // Show system bars again
            windowInsetsController.show(WindowInsetsCompat.Type.systemBars())

            Logger.debug(TAG, "Exited immersive mode")
        } catch (e: Exception) {
            Logger.error(TAG, "Failed to exit immersive mode: ${e.message}", null)
        }
    }

    // JNI callbacks to Rust
    private external fun onMiniAppOpened(miniappId: String, chatId: String, messageId: String)
    private external fun onMiniAppClosed(miniappId: String)
    private external fun onMiniAppCrashed(miniappId: String)
}
