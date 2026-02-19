package io.vectorapp

import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.webkit.WebSettings
import android.webkit.WebView
import android.os.Bundle
import android.view.View
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import io.vectorapp.miniapp.MiniAppManager

class MainActivity : TauriActivity() {

    companion object {
        private const val NOTIFICATION_PERMISSION_REQUEST_CODE = 1001

        init {
            System.loadLibrary("vector_lib")
        }

        @JvmStatic
        external fun nativeOnResume()
        @JvmStatic
        external fun nativeOnPause()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // Ensure hardware acceleration is enabled
        window.addFlags(android.view.WindowManager.LayoutParams.FLAG_HARDWARE_ACCELERATED)

        // Request notification permission (Android 13+)
        requestNotificationPermission()

        // Cancel any background WorkManager polling (app is now in foreground)
        VectorNotificationService.cancelPeriodicPolling(this)

        // Start the foreground notification service
        val serviceIntent = Intent(this, VectorNotificationService::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            startForegroundService(serviceIntent)
        } else {
            startService(serviceIntent)
        }
    }

    override fun onResume() {
        super.onResume()
        try { nativeOnResume() } catch (_: Exception) {}
    }

    override fun onPause() {
        super.onPause()
        try { nativeOnPause() } catch (_: Exception) {}
    }

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

    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        // If a Mini App is open, close it instead of navigating back
        if (MiniAppManager.isOpen()) {
            MiniAppManager.closeMiniApp()
            return
        }
        @Suppress("DEPRECATION")
        super.onBackPressed()
    }

    override fun onWebViewCreate(webView: WebView) {
        super.onWebViewCreate(webView)

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
            
            // Database settings
            databaseEnabled = true
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
    }
}