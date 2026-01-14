package io.vectorapp.miniapp

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.pm.PackageManager
import android.view.View
import android.webkit.GeolocationPermissions
import android.webkit.PermissionRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebChromeClient
import android.webkit.ConsoleMessage
import android.widget.FrameLayout
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import io.vectorapp.Logger

/**
 * Custom WebView for Mini Apps with security-focused settings.
 *
 * This WebView is designed to run WebXDC apps in a sandboxed environment
 * with strict security settings that match the desktop implementation.
 */
class MiniAppWebView(
    context: Context,
    private val miniappId: String,
    private val packagePath: String
) : WebView(context) {

    private lateinit var chromeClient: MiniAppChromeClient

    companion object {
        private const val TAG = "MiniAppWebView"

        /**
         * Initialization script injected before any page scripts run.
         * This provides security hardening and the webxdc API bridge.
         */
        private const val INIT_SCRIPT = """
            (function() {
                'use strict';
                console.log('[MiniApp] Initialization script running');

                // ========================================
                // Security: Disable WebRTC to prevent IP leaks
                // ========================================
                try {
                    window.RTCPeerConnection = function() {
                        throw new Error('WebRTC is disabled in Mini Apps');
                    };
                    window.webkitRTCPeerConnection = window.RTCPeerConnection;
                    window.mozRTCPeerConnection = window.RTCPeerConnection;
                } catch(e) {
                    console.warn('[MiniApp] Could not disable WebRTC:', e);
                }

                // ========================================
                // Security: Wrap media APIs with permission checks
                // ========================================
                (function() {
                    // Permission cache
                    let permissionCache = null;
                    let permissionCacheTime = 0;
                    const CACHE_TTL = 5000;

                    // Check permission via native IPC
                    async function checkPermission(permName) {
                        const now = Date.now();
                        if (permissionCache && (now - permissionCacheTime) < CACHE_TTL) {
                            return permissionCache.includes(permName);
                        }

                        try {
                            const granted = window.__MINIAPP_IPC__.invokeCommand(
                                'get_granted_permissions',
                                '{}'
                            );
                            permissionCache = granted ? granted.split(',').map(p => p.trim()) : [];
                            permissionCacheTime = now;
                            return permissionCache.includes(permName);
                        } catch(e) {
                            console.warn('[MiniApp] Permission check failed:', e);
                            return false;
                        }
                    }

                    function createNotAllowedError(message) {
                        return new DOMException(message, 'NotAllowedError');
                    }

                    // Wrap getUserMedia
                    const origGetUserMedia = navigator.mediaDevices?.getUserMedia?.bind(navigator.mediaDevices);
                    if (navigator.mediaDevices && origGetUserMedia) {
                        navigator.mediaDevices.getUserMedia = async function(constraints) {
                            if (constraints?.audio) {
                                const allowed = await checkPermission('microphone');
                                if (!allowed) {
                                    throw createNotAllowedError('Microphone permission denied by Vector');
                                }
                            }
                            if (constraints?.video) {
                                const allowed = await checkPermission('camera');
                                if (!allowed) {
                                    throw createNotAllowedError('Camera permission denied by Vector');
                                }
                            }
                            return origGetUserMedia(constraints);
                        };
                    }

                    // Wrap getDisplayMedia
                    const origGetDisplayMedia = navigator.mediaDevices?.getDisplayMedia?.bind(navigator.mediaDevices);
                    if (navigator.mediaDevices && origGetDisplayMedia) {
                        navigator.mediaDevices.getDisplayMedia = async function(constraints) {
                            const allowed = await checkPermission('display-capture');
                            if (!allowed) {
                                throw createNotAllowedError('Screen capture permission denied by Vector');
                            }
                            return origGetDisplayMedia(constraints);
                        };
                    }

                    // Wrap geolocation
                    const origGetCurrentPosition = navigator.geolocation?.getCurrentPosition?.bind(navigator.geolocation);
                    const origWatchPosition = navigator.geolocation?.watchPosition?.bind(navigator.geolocation);

                    if (navigator.geolocation && origGetCurrentPosition) {
                        navigator.geolocation.getCurrentPosition = async function(success, error, options) {
                            const allowed = await checkPermission('geolocation');
                            if (!allowed) {
                                if (error) {
                                    error({ code: 1, message: 'Geolocation permission denied by Vector', PERMISSION_DENIED: 1 });
                                }
                                return;
                            }
                            return origGetCurrentPosition(success, error, options);
                        };

                        navigator.geolocation.watchPosition = async function(success, error, options) {
                            const allowed = await checkPermission('geolocation');
                            if (!allowed) {
                                if (error) {
                                    error({ code: 1, message: 'Geolocation permission denied by Vector', PERMISSION_DENIED: 1 });
                                }
                                return 0;
                            }
                            return origWatchPosition(success, error, options);
                        };
                    }

                    // Wrap clipboard
                    const origReadText = navigator.clipboard?.readText?.bind(navigator.clipboard);
                    const origWriteText = navigator.clipboard?.writeText?.bind(navigator.clipboard);

                    if (navigator.clipboard) {
                        if (origReadText) {
                            navigator.clipboard.readText = async function() {
                                const allowed = await checkPermission('clipboard-read');
                                if (!allowed) {
                                    throw createNotAllowedError('Clipboard read permission denied by Vector');
                                }
                                return origReadText();
                            };
                        }
                        if (origWriteText) {
                            navigator.clipboard.writeText = async function(text) {
                                const allowed = await checkPermission('clipboard-write');
                                if (!allowed) {
                                    throw createNotAllowedError('Clipboard write permission denied by Vector');
                                }
                                return origWriteText(text);
                            };
                        }
                    }

                    console.log('[MiniApp] Media API permission guards installed');
                })();

                // ========================================
                // Realtime channel listener storage
                // ========================================
                window.__miniapp_realtime_listener = null;

                console.log('[MiniApp] Initialization complete');
            })();
        """
    }

    init {
        Logger.debug(TAG, "Creating MiniAppWebView for: $miniappId")

        // Enable hardware acceleration
        setLayerType(View.LAYER_TYPE_HARDWARE, null)

        // Full-screen layout
        layoutParams = FrameLayout.LayoutParams(
            FrameLayout.LayoutParams.MATCH_PARENT,
            FrameLayout.LayoutParams.MATCH_PARENT
        )

        // Configure security-focused settings
        settings.apply {
            // JavaScript is required for Mini Apps
            javaScriptEnabled = true

            // DOM storage for app state
            domStorageEnabled = true

            // Disable file access (security)
            allowFileAccess = false
            allowContentAccess = false

            // Mixed content - strict (no HTTP in HTTPS context)
            mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW

            // Disable geolocation by default (permission-gated)
            setGeolocationEnabled(false)

            // Media settings - allow autoplay for games
            mediaPlaybackRequiresUserGesture = false

            // Cache for performance
            cacheMode = WebSettings.LOAD_DEFAULT
            databaseEnabled = true

            // Viewport settings
            useWideViewPort = true
            loadWithOverviewMode = true

            // Text encoding
            defaultTextEncodingName = "UTF-8"

            // Disable zoom (Mini Apps are fixed-size)
            setSupportZoom(false)
            builtInZoomControls = false
            displayZoomControls = false
        }

        // Add JavaScript interface for IPC
        addJavascriptInterface(
            MiniAppIpc(miniappId, packagePath),
            "__MINIAPP_IPC__"
        )

        // Set custom WebViewClient for protocol handling
        webViewClient = MiniAppWebViewClient(context, miniappId, packagePath)

        // Set WebChromeClient for console logging and permission handling
        chromeClient = MiniAppChromeClient(context, miniappId)
        webChromeClient = chromeClient

        // Inject initialization script
        evaluateJavascript(INIT_SCRIPT, null)

        Logger.debug(TAG, "MiniAppWebView configured")
    }

    /**
     * Forward permission results from Activity.onRequestPermissionsResult()
     */
    fun handlePermissionResult(requestCode: Int, grantResults: IntArray) {
        chromeClient.handlePermissionResult(requestCode, grantResults)
    }

    override fun destroy() {
        Logger.debug(TAG, "Destroying MiniAppWebView: $miniappId")

        // Clear JavaScript interface
        removeJavascriptInterface("__MINIAPP_IPC__")

        // Clear WebView state
        stopLoading()
        clearHistory()
        clearCache(true)
        loadUrl("about:blank")

        // Remove from parent if attached
        (parent as? android.view.ViewGroup)?.removeView(this)

        super.destroy()
    }

    /**
     * WebChromeClient for Mini Apps with permission handling.
     */
    private class MiniAppChromeClient(
        private val context: Context,
        private val miniappId: String
    ) : WebChromeClient() {

        companion object {
            private const val TAG = "MiniAppChromeClient"

            /**
             * Base request code for Android permission requests from Mini Apps.
             * Using a high, arbitrary value (9877) to avoid conflicts with other
             * permission request codes in the app. Geolocation uses +1 (9878).
             */
            private const val PERMISSION_REQUEST_CODE = 9877
        }

        // Pending permission request from web content
        private var pendingPermissionRequest: PermissionRequest? = null
        private var pendingGeoCallback: GeolocationPermissions.Callback? = null
        private var pendingGeoOrigin: String? = null

        override fun onConsoleMessage(consoleMessage: ConsoleMessage?): Boolean {
            consoleMessage?.let { msg ->
                val logMsg = "[MiniApp:$miniappId] ${msg.message()} (${msg.sourceId()}:${msg.lineNumber()})"
                when (msg.messageLevel()) {
                    ConsoleMessage.MessageLevel.ERROR -> Logger.error(TAG, logMsg, null)
                    ConsoleMessage.MessageLevel.WARNING -> Logger.warn(TAG, logMsg)
                    ConsoleMessage.MessageLevel.DEBUG -> Logger.debug(TAG, logMsg)
                    else -> Logger.info(TAG, logMsg)
                }
            }
            return true
        }

        /**
         * Handle permission requests from web content (camera, microphone).
         */
        override fun onPermissionRequest(request: PermissionRequest?) {
            request?.let { req ->
                Logger.info(TAG, "Permission request for: ${req.resources.joinToString()}")

                val activity = context as? Activity
                if (activity == null) {
                    Logger.warn(TAG, "No activity context, denying permission")
                    req.deny()
                    return
                }

                // Map WebView permissions to Android permissions
                val androidPermissions = mutableListOf<String>()
                val grantedResources = mutableListOf<String>()

                for (resource in req.resources) {
                    when (resource) {
                        PermissionRequest.RESOURCE_AUDIO_CAPTURE -> {
                            if (hasPermission(Manifest.permission.RECORD_AUDIO)) {
                                grantedResources.add(resource)
                            } else {
                                androidPermissions.add(Manifest.permission.RECORD_AUDIO)
                            }
                        }
                        PermissionRequest.RESOURCE_VIDEO_CAPTURE -> {
                            if (hasPermission(Manifest.permission.CAMERA)) {
                                grantedResources.add(resource)
                            } else {
                                androidPermissions.add(Manifest.permission.CAMERA)
                            }
                        }
                        // Other resources are denied by default for security
                        else -> Logger.warn(TAG, "Denying unsupported resource: $resource")
                    }
                }

                // If all requested permissions are already granted
                if (androidPermissions.isEmpty() && grantedResources.isNotEmpty()) {
                    Logger.info(TAG, "Granting already-permitted resources: ${grantedResources.joinToString()}")
                    req.grant(grantedResources.toTypedArray())
                    return
                }

                // If we need to request Android permissions
                if (androidPermissions.isNotEmpty()) {
                    pendingPermissionRequest = req
                    Logger.info(TAG, "Requesting Android permissions: ${androidPermissions.joinToString()}")
                    ActivityCompat.requestPermissions(
                        activity,
                        androidPermissions.toTypedArray(),
                        PERMISSION_REQUEST_CODE
                    )
                    return
                }

                // No valid resources to grant
                Logger.warn(TAG, "No valid resources to grant, denying request")
                req.deny()
            }
        }

        override fun onPermissionRequestCanceled(request: PermissionRequest?) {
            Logger.debug(TAG, "Permission request canceled")
            if (pendingPermissionRequest == request) {
                pendingPermissionRequest = null
            }
        }

        /**
         * Handle geolocation permission requests.
         */
        override fun onGeolocationPermissionsShowPrompt(
            origin: String?,
            callback: GeolocationPermissions.Callback?
        ) {
            Logger.info(TAG, "Geolocation permission request from: $origin")

            val activity = context as? Activity
            if (activity == null || origin == null || callback == null) {
                callback?.invoke(origin, false, false)
                return
            }

            // Check if we already have location permission
            if (hasPermission(Manifest.permission.ACCESS_FINE_LOCATION)) {
                Logger.info(TAG, "Geolocation already permitted")
                callback.invoke(origin, true, false)
                return
            }

            // Request location permission
            pendingGeoCallback = callback
            pendingGeoOrigin = origin
            ActivityCompat.requestPermissions(
                activity,
                arrayOf(Manifest.permission.ACCESS_FINE_LOCATION),
                PERMISSION_REQUEST_CODE + 1
            )
        }

        override fun onGeolocationPermissionsHidePrompt() {
            Logger.debug(TAG, "Geolocation prompt hidden")
            pendingGeoCallback = null
            pendingGeoOrigin = null
        }

        private fun hasPermission(permission: String): Boolean {
            return ContextCompat.checkSelfPermission(context, permission) ==
                PackageManager.PERMISSION_GRANTED
        }

        /**
         * Called when Android permission result is received.
         * This should be called from MainActivity.onRequestPermissionsResult()
         */
        fun handlePermissionResult(requestCode: Int, grantResults: IntArray) {
            when (requestCode) {
                PERMISSION_REQUEST_CODE -> {
                    // Handle camera/mic permission result
                    pendingPermissionRequest?.let { req ->
                        val allGranted = grantResults.isNotEmpty() &&
                            grantResults.all { it == PackageManager.PERMISSION_GRANTED }

                        if (allGranted) {
                            Logger.info(TAG, "Permission granted, allowing web request")
                            req.grant(req.resources)
                        } else {
                            Logger.info(TAG, "Permission denied")
                            req.deny()
                        }
                        pendingPermissionRequest = null
                    }
                }
                PERMISSION_REQUEST_CODE + 1 -> {
                    // Handle geolocation permission result
                    val granted = grantResults.isNotEmpty() &&
                        grantResults[0] == PackageManager.PERMISSION_GRANTED

                    pendingGeoCallback?.invoke(pendingGeoOrigin, granted, false)
                    pendingGeoCallback = null
                    pendingGeoOrigin = null
                }
            }
        }
    }
}
