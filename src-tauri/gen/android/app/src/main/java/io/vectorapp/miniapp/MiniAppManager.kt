package io.vectorapp.miniapp

import android.app.Activity
import android.view.ViewGroup
import android.view.animation.AccelerateInterpolator
import android.view.animation.DecelerateInterpolator
import android.util.Base64
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import io.vectorapp.Logger

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
     * Uses Base64 encoding for efficient binary transfer (matches JS→Kotlin direction).
     */
    @JvmStatic
    fun sendRealtimeData(data: ByteArray) {
        val webView = currentMiniApp?.webView ?: return

        activity?.runOnUiThread {
            // Encode as Base64 for efficient transfer (avoids large string allocations)
            val base64 = Base64.encodeToString(data, Base64.NO_WRAP)

            val script = """
                (function() {
                    try {
                        if (window.__miniapp_realtime_listener) {
                            // Decode Base64 to Uint8Array
                            const binary = atob('$base64');
                            const bytes = new Uint8Array(binary.length);
                            for (let i = 0; i < binary.length; i++) {
                                bytes[i] = binary.charCodeAt(i);
                            }
                            window.__miniapp_realtime_listener(bytes);
                        }
                    } catch(e) {
                        console.error('Failed to deliver realtime data:', e);
                    }
                })();
            """.trimIndent()

            webView.evaluateJavascript(script, null)
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
}
