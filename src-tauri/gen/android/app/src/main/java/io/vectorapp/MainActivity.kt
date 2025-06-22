package io.vectorapp

import android.webkit.WebSettings
import android.webkit.WebView
import android.os.Bundle
import android.view.View

class MainActivity : TauriActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        
        // Ensure hardware acceleration is enabled
        window.addFlags(android.view.WindowManager.LayoutParams.FLAG_HARDWARE_ACCELERATED)
    }

    override fun onWebViewCreate(webView: WebView) {
        super.onWebViewCreate(webView)
        
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
        PermissionHandler.handlePermissionResult(
            requestCode, 
            permissions.toList().toTypedArray(), 
            grantResults
        )
    }
}