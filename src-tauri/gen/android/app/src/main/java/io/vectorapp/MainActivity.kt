package io.vectorapp

import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Rect
import android.net.Uri
import android.os.Build
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.webkit.JavascriptInterface
import android.webkit.WebSettings
import android.webkit.WebView
import android.os.Bundle
import android.view.View
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import io.vectorapp.miniapp.MiniAppManager


/** Chromium the shared frontend needs (Vector Web's preflight floor). */
private const val MIN_WEBVIEW = 98

class MainActivity : TauriActivity() {

    companion object {
        private const val NOTIFICATION_PERMISSION_REQUEST_CODE = 1001

        init {
            System.loadLibrary("vector_lib")
        }

        @JvmStatic
        external fun nativeOnResume()

        @JvmStatic
        external fun nativeRegisterNdkContext(activity: android.app.Activity)
        @JvmStatic
        external fun nativeOnPause()
        @JvmStatic
        external fun nativeOnNotificationTap(chatId: String)
        @JvmStatic
        external fun nativeOnCallTap(callId: String, npub: String, answer: Boolean)
        @JvmStatic
        external fun nativeOnShareReceived(uris: Array<String>, text: String)
    }

    private var managedWebView: WebView? = null

    // ===== Device-tilt for the badge card =====
    // Started/stopped from JS (window.__vectorGyroBridge) only while the card is open, so the sensor
    // costs no battery otherwise. Uses the GRAVITY vector (preferred) or the raw ACCELEROMETER (present
    // on every phone) — the device's angle relative to gravity — captured flat at the first reading,
    // then pushed as window.__vectorGyro(rx, ry) deltas. (Not the gyroscope: that's motion-only and
    // absent on many devices.)
    private var sensorManager: SensorManager? = null
    private var rotationSensor: Sensor? = null      // gravity (preferred) or raw accelerometer
    private var gyroActive = false                  // listener currently registered
    private var gyroWanted = false                  // JS wants tilt (card open) — drives re-arm on resume
    private var gyroHasBaseline = false
    private var gravX = 0.0                          // low-pass-filtered gravity vector
    private var gravY = 0.0
    private var gravZ = 0.0
    private var baseNx = 0.0                         // normalised gravity at the first reading = "flat"
    private var baseNy = 0.0
    private var gyroLastPushMs = 0L

    private val gyroListener = object : SensorEventListener {
        override fun onSensorChanged(event: SensorEvent) {
            if (!gyroHasBaseline) {
                gravX = event.values[0].toDouble(); gravY = event.values[1].toDouble(); gravZ = event.values[2].toDouble()
                val m0 = Math.sqrt(gravX * gravX + gravY * gravY + gravZ * gravZ)
                if (m0 < 1e-3) return                 // wait for a valid first reading
                baseNx = gravX / m0; baseNy = gravY / m0
                gyroHasBaseline = true
                return
            }
            // Low-pass to isolate gravity from hand-motion (raw accel) and damp jitter (gravity sensor).
            val a = 0.15
            gravX += (event.values[0].toDouble() - gravX) * a
            gravY += (event.values[1].toDouble() - gravY) * a
            gravZ += (event.values[2].toDouble() - gravZ) * a
            val mag = Math.sqrt(gravX * gravX + gravY * gravY + gravZ * gravZ)
            if (mag < 1e-3) return
            val now = System.currentTimeMillis()
            if (now - gyroLastPushMs < 30) return     // ~33 Hz cap on the JS bridge
            gyroLastPushMs = now
            // Normalised gravity vs the baseline → tilt. On this device the screen-plane axes map
            // swapped: Y drives forward/back, X drives side-to-side; Z (screen rotation) is ignored.
            // Gain maps ~25 deg of phone tilt to the card's full range.
            val nx = gravX / mag
            val ny = gravY / mag
            var rx = -(ny - baseNy) * 28.0   // forward/back <- Y
            var ry = (nx - baseNx) * 28.0    // side-to-side <- X
            if (Math.abs(rx) < 0.5) rx = 0.0
            if (Math.abs(ry) < 0.5) ry = 0.0
            val wv = managedWebView ?: return
            wv.post { wv.evaluateJavascript("window.__vectorGyro && window.__vectorGyro($rx, $ry)", null) }
        }
        override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) {}
    }

    inner class GyroBridge {
        // Returns whether a usable tilt sensor exists; JS falls back to touch tilt when false. Stays
        // read-only on this binder thread (no field writes); startGyro() (UI thread) caches + registers.
        @JavascriptInterface fun start(): Boolean {
            val sm = sensorManager ?: getSystemService(SensorManager::class.java) ?: return false
            val present = sm.getDefaultSensor(Sensor.TYPE_GRAVITY) != null
                || sm.getDefaultSensor(Sensor.TYPE_ACCELEROMETER) != null
            if (present) runOnUiThread { gyroWanted = true; startGyro() }
            return present
        }
        @JavascriptInterface fun stop() { runOnUiThread { gyroWanted = false; stopGyro() } }
    }

    private fun startGyro() {
        if (gyroActive) return
        val sm = sensorManager
            ?: getSystemService(SensorManager::class.java)?.also { sensorManager = it }
            ?: return
        val sensor = rotationSensor
            ?: sm.getDefaultSensor(Sensor.TYPE_GRAVITY)
            ?: sm.getDefaultSensor(Sensor.TYPE_ACCELEROMETER)
            ?: return
        rotationSensor = sensor
        gyroHasBaseline = false
        sm.registerListener(gyroListener, sensor, SensorManager.SENSOR_DELAY_GAME)
        gyroActive = true
    }

    private fun stopGyro() {
        if (!gyroActive) return
        sensorManager?.unregisterListener(gyroListener)
        gyroActive = false
    }

    /**
     * The frontend needs Chromium 98, as Vector Web does. A phone whose WebView never updated
     * (no app store, or updates off) would show a blank app, so it says what to update instead.
     */
    private fun warnIfWebViewTooOld() {
        val webView = WebView.getCurrentWebViewPackage() ?: return
        val major = webView.versionName?.substringBefore('.')?.toIntOrNull() ?: return
        if (major >= MIN_WEBVIEW) return
        android.app.AlertDialog.Builder(this)
            .setTitle("Update Android System WebView")
            .setMessage("Vector needs version $MIN_WEBVIEW or newer, and this phone has $major. Update it, then reopen Vector.")
            .setPositiveButton("Update") { _, _ ->
                val store = Intent(Intent.ACTION_VIEW, Uri.parse("market://details?id=${webView.packageName}"))
                val web = Intent(Intent.ACTION_VIEW, Uri.parse("https://play.google.com/store/apps/details?id=${webView.packageName}"))
                try { startActivity(store) } catch (_: Exception) { try { startActivity(web) } catch (_: Exception) {} }
            }
            .setNegativeButton("Close", null)
            .show()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        blockWebViewDiskCache()
        super.onCreate(savedInstanceState)
        warnIfWebViewTooOld()

        // Hand Rust the activity for ndk_context (tao 0.35 stopped registering it).
        try { nativeRegisterNdkContext(this) } catch (e: Throwable) {
            android.util.Log.e("MainActivity", "nativeRegisterNdkContext failed", e)
        }

        // Ensure hardware acceleration is enabled
        window.addFlags(android.view.WindowManager.LayoutParams.FLAG_HARDWARE_ACCELERATED)

        // Request notification permission (Android 13+)
        requestNotificationPermission()

        // Start the foreground notification service (only if user hasn't disabled it)
        if (VectorBatteryHelper.getBackgroundServiceEnabled(this)) {
            val serviceIntent = Intent(this, VectorNotificationService::class.java)
            startForegroundService(serviceIntent)
        }

        // Handle notification tap that launched the app
        handleNotificationIntent(intent)
        // Handle a file/text share that launched the app
        handleSendIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // Handle notification tap when app is already running
        handleNotificationIntent(intent)
        // Handle a share that arrived while the app was running
        handleSendIntent(intent)
    }

    override fun onResume() {
        super.onResume()
        if (gyroWanted) startGyro()   // re-arm the tilt sensor if the badge card is still open
        try { nativeOnResume() } catch (_: Exception) {}
        // Clear notification message history — user is in the app, stale history is irrelevant
        VectorNotificationService.clearAllMessageHistory()
    }

    override fun onPause() {
        super.onPause()
        stopGyro()   // never leave the rotation sensor running while backgrounded
        try { nativeOnPause() } catch (_: Exception) {}
    }

    override fun onStop() {
        super.onStop()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
            setShowWhenLocked(false)
            setTurnScreenOn(false)
        }
        // Whatever an embedded player's pages left in the webview's cache goes with the app.
        managedWebView?.clearCache(true)
    }

    /**
     * Pages the webview loads from the network (the embedded players) keep no HTTP cache on
     * disk: a file stands where its cache folder would be, so the cache can't open. Runs
     * before the webview exists. Local storage and IndexedDB are untouched.
     */
    private fun blockWebViewDiskCache() {
        try {
            val cache = java.io.File(cacheDir, "WebView/Default/HTTP Cache")
            if (cache.isDirectory) cache.deleteRecursively()
            if (!cache.exists()) {
                cache.parentFile?.mkdirs()
                cache.createNewFile()
            }
        } catch (e: Exception) {
            android.util.Log.w("MainActivity", "webview cache block failed", e)
        }
    }

    private fun handleNotificationIntent(intent: Intent?) {
        val callNpub = intent?.getStringExtra(IncomingCall.EXTRA_CALL_NPUB)
        if (!callNpub.isNullOrEmpty()) {
            val callId = intent.getStringExtra(IncomingCall.EXTRA_CALL_ID) ?: ""
            val answer = intent.getBooleanExtra(IncomingCall.EXTRA_CALL_ANSWER, false)
            intent.removeExtra(IncomingCall.EXTRA_CALL_NPUB)
            // A ringing call shows over the lock screen; leaving the app takes that back.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
                setShowWhenLocked(true)
                setTurnScreenOn(true)
            }
            try { nativeOnCallTap(callId, callNpub, answer) } catch (_: Exception) {}
            return
        }
        val chatId = intent?.getStringExtra("chat_id")
        if (!chatId.isNullOrEmpty()) {
            intent.removeExtra("chat_id") // Consume to prevent re-processing
            android.util.Log.d("MainActivity", "Notification tap → chat: ${chatId.take(20)}")
            try { nativeOnNotificationTap(chatId) } catch (_: Exception) {}
        }
    }

    /**
     * Handle another app sharing files/text into Vector via the share sheet.
     * Extracts content:// URIs (single or multiple) plus any plain text and
     * hands them to native; the frontend then lets the user pick a chat.
     */
    private fun handleSendIntent(intent: Intent?) {
        if (intent == null) return
        val action = intent.action ?: return
        if (action != Intent.ACTION_SEND && action != Intent.ACTION_SEND_MULTIPLE) return

        val uris = ArrayList<String>()
        var text = ""
        if (action == Intent.ACTION_SEND) {
            streamUri(intent)?.let { uris.add(it.toString()) }
            intent.getStringExtra(Intent.EXTRA_TEXT)?.let { text = it }
        } else {
            streamUris(intent)?.forEach { uris.add(it.toString()) }
        }

        // Consume so a rotation/relaunch doesn't re-share the same payload.
        intent.action = null
        intent.removeExtra(Intent.EXTRA_STREAM)
        intent.removeExtra(Intent.EXTRA_TEXT)

        if (uris.isEmpty() && text.isEmpty()) return
        try { nativeOnShareReceived(uris.toTypedArray(), text) } catch (_: Throwable) {}
    }

    @Suppress("DEPRECATION")
    private fun streamUri(intent: Intent): Uri? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU)
            intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
        else
            intent.getParcelableExtra(Intent.EXTRA_STREAM)

    @Suppress("DEPRECATION")
    private fun streamUris(intent: Intent): ArrayList<Uri>? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU)
            intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java)
        else
            intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM)

    private fun requestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            if (ContextCompat.checkSelfPermission(
                    this,
                    android.Manifest.permission.POST_NOTIFICATIONS
                ) != PackageManager.PERMISSION_GRANTED
            ) {
                ActivityCompat.requestPermissions(
                    this,
                    arrayOf(android.Manifest.permission.POST_NOTIFICATIONS),
                    NOTIFICATION_PERMISSION_REQUEST_CODE
                )
            }
        }
    }

    /**
     * Vector draws its own side swipes (the Communities sidebar, swipe to reply), so the system
     * back gesture gives way along both side edges as far as Android allows: 200dp of height a
     * side. On the left it sits on the Communities tab while one shows, else where a thumb
     * swipes. Back itself still reaches the app through onBackPressed.
     */
    private fun claimSideEdges(view: View) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
        view.addOnLayoutChangeListener { v, _, _, _, _, _, _, _, _ -> applyEdges(v) }
    }

    /** The page's left-edge pull (top, bottom) in the WebView's pixels, while it shows one. */
    private var leftPull: Pair<Int, Int>? = null

    inner class EdgeBridge {
        @JavascriptInterface fun claimLeft(top: Float, bottom: Float) {
            runOnUiThread {
                val density = resources.displayMetrics.density
                leftPull = (top * density).toInt() to (bottom * density).toInt()
                managedWebView?.let { applyEdges(it) }
            }
        }
        @JavascriptInterface fun releaseLeft() {
            runOnUiThread {
                leftPull = null
                managedWebView?.let { applyEdges(it) }
            }
        }
    }

    private fun applyEdges(v: View) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
        val density = v.resources.displayMetrics.density
        val (width, height) = v.width to v.height
        val band = (200 * density).toInt().coerceAtMost(height)
        val edge = (48 * density).toInt()
        val thumb = (height * 2 / 5 - band / 2).coerceIn(0, height - band)
        val left = leftPull?.let { (top, bottom) -> ((top + bottom) / 2 - band / 2).coerceIn(0, height - band) } ?: thumb
        v.systemGestureExclusionRects = listOf(
            Rect(0, left, edge, left + band),
            Rect(width - edge, thumb, width, thumb + band),
        )
    }

    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        // If a Mini App is open, close it instead of navigating back
        if (MiniAppManager.isOpen()) {
            MiniAppManager.closeMiniApp()
            return
        }

        // Ask the web frontend if it can pop its own nav stack (chats,
        // overviews, settings tabs, custom modals). Falls back to
        // moveTaskToBack(true) so the activity hides without tearing down
        // state — matches native Android "back" semantics on a root screen.
        val wv = managedWebView
        if (wv == null) {
            @Suppress("DEPRECATION")
            super.onBackPressed()
            return
        }
        wv.evaluateJavascript(
            "(window.__vectorOnAndroidBack && window.__vectorOnAndroidBack()) ? 'handled' : 'unhandled'"
        ) { result ->
            val handled = result?.trim('"') == "handled"
            if (!handled) {
                moveTaskToBack(true)
            }
        }
    }

    override fun onWebViewCreate(webView: WebView) {
        super.onWebViewCreate(webView)
        managedWebView = webView

        claimSideEdges(webView)

        // Expose the gyro bridge to the badge card: JS calls window.__vectorGyroBridge.start()/stop().
        webView.addJavascriptInterface(GyroBridge(), "__vectorGyroBridge")
        webView.addJavascriptInterface(EdgeBridge(), "__vectorEdges")

        // Initialize MiniAppManager for Mini Apps overlay support
        MiniAppManager.initialize(this)
        
        // Enable hardware acceleration
        webView.setLayerType(View.LAYER_TYPE_NONE, null)
        
        // Configure WebView settings for better video support
        webView.settings.apply {
            // Basic settings
            domStorageEnabled = true
            
            // File access settings
            allowFileAccess = true
            allowContentAccess = true
            
            // Media playback settings
            mediaPlaybackRequiresUserGesture = false
            
            // Mixed content mode
            mixedContentMode = WebSettings.MIXED_CONTENT_ALWAYS_ALLOW
            
            // Cache settings
            cacheMode = WebSettings.LOAD_DEFAULT
            
            // Viewport settings
            useWideViewPort = true
            loadWithOverviewMode = true
        }
    }
    
    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)

        // Forward to MiniAppManager for Mini App permission requests
        MiniAppManager.handlePermissionResult(requestCode, grantResults)

        // Wake the native blocking waiter for RECORD_AUDIO (voice recording).
        // Without this the Rust condvar in request_audio_permission_blocking
        // never resolves and the send times out after 60s.
        PermissionHandler.handlePermissionResult(requestCode, grantResults)
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        // Wake the native NIP-55 signer waiter for a returned Amber intent.
        ExternalSigner.handleActivityResult(requestCode, resultCode, data)
    }
}