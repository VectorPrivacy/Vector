package io.vectorapp.miniapp

import android.content.Context
import android.graphics.Bitmap
import android.net.http.SslError
import android.webkit.SslErrorHandler
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import io.vectorapp.Logger
import java.io.ByteArrayInputStream

/**
 * Custom WebViewClient that intercepts requests to webxdc.localhost
 * and serves files from the Mini App package via JNI.
 */
class MiniAppWebViewClient(
    private val context: Context,
    private val miniappId: String,
    private val packagePath: String
) : WebViewClient() {

    companion object {
        private const val TAG = "MiniAppWebViewClient"

        /** The custom protocol host for Mini Apps (Android requires http://) */
        private const val WEBXDC_HOST = "webxdc.localhost"

        /**
         * Content Security Policy for Mini Apps.
         *
         * Security directives:
         * - `default-src 'self'`: Only allow resources from webxdc.localhost
         * - `webrtc 'block'`: Prevent IP leaks via WebRTC peer connections
         * - `connect-src`: No external network (only self, ipc, data, blob)
         *
         * Permissive directives (required for Mini Apps to function):
         * - `unsafe-inline`: Many apps use inline scripts/styles
         * - `unsafe-eval`: Some apps use eval() or Function()
         * - `blob:`: Apps may create blob URLs for generated content
         *
         * This matches the desktop implementation for consistency.
         */
        private const val CSP = """default-src 'self' http://webxdc.localhost; style-src 'self' http://webxdc.localhost 'unsafe-inline' blob:; font-src 'self' http://webxdc.localhost data: blob:; script-src 'self' http://webxdc.localhost 'unsafe-inline' 'unsafe-eval' blob:; connect-src 'self' http://webxdc.localhost ipc: data: blob:; img-src 'self' http://webxdc.localhost data: blob:; media-src 'self' http://webxdc.localhost data: blob:; webrtc 'block'"""

        /**
         * Permissions Policy that denies all sensitive APIs by default.
         * Mini Apps must request permissions through the Vector permission system.
         */
        private const val PERMISSIONS_POLICY = """accelerometer=(), ambient-light-sensor=(), autoplay=(), battery=(), bluetooth=(), camera=(), clipboard-read=(), clipboard-write=(), display-capture=(), fullscreen=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), midi=(), payment=(), picture-in-picture=(), screen-wake-lock=(), speaker-selection=(), usb=(), web-share=(), xr-spatial-tracking=()"""

        /**
         * webxdc.js bridge script that provides the window.webxdc API.
         */
        private fun generateWebxdcJs(selfAddr: String, selfName: String): String {
            return """
                // Mini App Bridge for Vector (Android)
                (function() {
                    'use strict';

                    const selfAddr = ${selfAddr.toJsString()};
                    const selfName = ${selfName.toJsString()};

                    // State tracking
                    let updateListener = null;
                    let lastKnownSerial = 0;

                    // The Mini App API
                    window.webxdc = {
                        selfAddr: selfAddr,
                        selfName: selfName,

                        setUpdateListener: function(listener, serial) {
                            updateListener = listener;
                            lastKnownSerial = serial || 0;

                            // Request updates since last known serial
                            try {
                                const updates = window.__MINIAPP_IPC__.getUpdates(lastKnownSerial);
                                if (updates && updateListener) {
                                    const parsed = JSON.parse(updates);
                                    parsed.forEach(function(update) {
                                        updateListener(update);
                                    });
                                }
                            } catch(e) {
                                console.error('Failed to get updates:', e);
                            }

                            return Promise.resolve();
                        },

                        sendUpdate: function(update, description) {
                            return new Promise(function(resolve, reject) {
                                try {
                                    window.__MINIAPP_IPC__.sendUpdate(
                                        JSON.stringify(update),
                                        description || ''
                                    );
                                    resolve();
                                } catch(e) {
                                    reject(e);
                                }
                            });
                        },

                        sendToChat: function(content) {
                            console.warn('sendToChat is not yet implemented');
                            return Promise.reject(new Error('Not implemented'));
                        },

                        importFiles: function(filters) {
                            console.warn('importFiles is not yet implemented');
                            return Promise.reject(new Error('Not implemented'));
                        },

                        joinRealtimeChannel: function() {
                            console.log('[webxdc] joinRealtimeChannel called');

                            // Create the channel object synchronously (per WebXDC spec)
                            const channel = {
                                _listener: null,

                                setListener: function(listener) {
                                    this._listener = listener;
                                    // Store globally for native callback
                                    window.__miniapp_realtime_listener = listener;
                                },

                                send: function(data) {
                                    if (!(data instanceof Uint8Array)) {
                                        throw new Error('realtime data must be a Uint8Array');
                                    }
                                    if (data.length > 128000) {
                                        throw new Error('realtime data exceeds maximum size of 128000 bytes');
                                    }

                                    // Convert to base64 for JNI transfer
                                    let binary = '';
                                    const bytes = new Uint8Array(data);
                                    for (let i = 0; i < bytes.byteLength; i++) {
                                        binary += String.fromCharCode(bytes[i]);
                                    }
                                    const base64 = btoa(binary);

                                    try {
                                        window.__MINIAPP_IPC__.sendRealtimeData(base64);
                                    } catch(e) {
                                        console.error('Failed to send realtime data:', e);
                                    }
                                },

                                leave: function() {
                                    this._listener = null;
                                    window.__miniapp_realtime_listener = null;
                                    try {
                                        window.__MINIAPP_IPC__.leaveRealtimeChannel();
                                    } catch(e) {
                                        console.error('Failed to leave realtime channel:', e);
                                    }
                                }
                            };

                            // Join the channel on the backend
                            try {
                                const topicId = window.__MINIAPP_IPC__.joinRealtimeChannel();
                                if (topicId) {
                                    console.log('[webxdc] Joined realtime channel:', topicId);
                                }
                            } catch(e) {
                                console.error('Failed to join realtime channel:', e);
                            }

                            return channel;
                        }
                    };

                    console.log('[webxdc] Mini App bridge initialized');
                })();
            """.trimIndent()
        }

        private fun String.toJsString(): String {
            val escaped = this
                .replace("\\", "\\\\")
                .replace("\"", "\\\"")
                .replace("\n", "\\n")
                .replace("\r", "\\r")
                .replace("\t", "\\t")
            return "\"$escaped\""
        }

        init {
            System.loadLibrary("vector_lib")
        }
    }

    override fun shouldInterceptRequest(
        view: WebView?,
        request: WebResourceRequest?
    ): WebResourceResponse? {
        val url = request?.url ?: return null
        val host = url.host ?: return null

        // Only intercept webxdc.localhost requests
        if (host != WEBXDC_HOST) {
            Logger.warn(TAG, "[$miniappId] Blocked request to external host: $host")
            return createBlockedResponse()
        }

        val path = url.path ?: "/"
        Logger.debug(TAG, "[$miniappId] Intercepting request: $path")

        // Special handling for webxdc.js
        if (path == "/webxdc.js") {
            return serveWebxdcJs()
        }

        // Serve file from package via JNI
        return try {
            val response = handleMiniAppRequest(miniappId, packagePath, path)
            if (response != null) {
                response
            } else {
                Logger.warn(TAG, "[$miniappId] File not found: $path")
                createNotFoundResponse()
            }
        } catch (e: Exception) {
            Logger.error(TAG, "[$miniappId] Error serving file $path: ${e.message}", null)
            createErrorResponse()
        }
    }

    override fun shouldOverrideUrlLoading(
        view: WebView?,
        request: WebResourceRequest?
    ): Boolean {
        val url = request?.url?.toString() ?: return true

        // Only allow navigation within webxdc.localhost
        val allowed = url.startsWith("http://webxdc.localhost/") ||
                      url.startsWith("http://$WEBXDC_HOST/")

        if (!allowed) {
            Logger.warn(TAG, "[$miniappId] Blocked navigation to: $url")
        }

        return !allowed // Return true to block, false to allow
    }

    override fun onPageStarted(view: WebView?, url: String?, favicon: Bitmap?) {
        super.onPageStarted(view, url, favicon)
        Logger.debug(TAG, "[$miniappId] Page started: $url")
    }

    override fun onPageFinished(view: WebView?, url: String?) {
        super.onPageFinished(view, url)
        Logger.debug(TAG, "[$miniappId] Page finished: $url")
    }

    /**
     * Handle loading errors (network errors, etc).
     */
    override fun onReceivedError(
        view: WebView?,
        request: WebResourceRequest?,
        error: WebResourceError?
    ) {
        val errorCode = error?.errorCode ?: -1
        val errorDesc = error?.description?.toString() ?: "Unknown error"
        val url = request?.url?.toString() ?: "unknown"

        Logger.error(TAG, "[$miniappId] Load error for $url: $errorCode - $errorDesc", null)

        // Only show error page for main frame
        if (request?.isForMainFrame == true) {
            view?.loadData(
                createErrorPage("Loading Error", "Failed to load: $errorDesc"),
                "text/html",
                "UTF-8"
            )
        }
    }

    /**
     * Handle SSL errors - always cancel for security.
     */
    override fun onReceivedSslError(
        view: WebView?,
        handler: SslErrorHandler?,
        error: SslError?
    ) {
        Logger.error(TAG, "[$miniappId] SSL error: ${error?.primaryError}", null)
        // Always cancel SSL errors for security
        handler?.cancel()
    }

    /**
     * Serve the webxdc.js bridge script.
     */
    private fun serveWebxdcJs(): WebResourceResponse {
        // Get user info for selfAddr and selfName
        val selfAddr = try {
            getSelfAddrNative() ?: "unknown"
        } catch (e: Exception) {
            "unknown"
        }

        val selfName = try {
            getSelfNameNative() ?: "Unknown"
        } catch (e: Exception) {
            "Unknown"
        }

        val js = generateWebxdcJs(selfAddr, selfName)

        return WebResourceResponse(
            "text/javascript",
            "UTF-8",
            200,
            "OK",
            mapOf(
                "Content-Security-Policy" to CSP,
                "Permissions-Policy" to PERMISSIONS_POLICY,
                "X-Content-Type-Options" to "nosniff",
                "Cache-Control" to "no-cache"
            ),
            ByteArrayInputStream(js.toByteArray(Charsets.UTF_8))
        )
    }

    /**
     * Create a response for blocked requests.
     */
    private fun createBlockedResponse(): WebResourceResponse {
        return WebResourceResponse(
            "text/plain",
            "UTF-8",
            403,
            "Forbidden",
            mapOf(
                "Content-Security-Policy" to CSP,
                "Permissions-Policy" to PERMISSIONS_POLICY,
                "X-Content-Type-Options" to "nosniff"
            ),
            ByteArrayInputStream("Access denied".toByteArray())
        )
    }

    /**
     * Create a 404 response.
     */
    private fun createNotFoundResponse(): WebResourceResponse {
        return WebResourceResponse(
            "text/plain",
            "UTF-8",
            404,
            "Not Found",
            mapOf(
                "Content-Security-Policy" to CSP,
                "Permissions-Policy" to PERMISSIONS_POLICY,
                "X-Content-Type-Options" to "nosniff"
            ),
            ByteArrayInputStream("File not found".toByteArray())
        )
    }

    /**
     * Create a 500 error response.
     */
    private fun createErrorResponse(): WebResourceResponse {
        return WebResourceResponse(
            "text/plain",
            "UTF-8",
            500,
            "Internal Server Error",
            mapOf(
                "Content-Security-Policy" to CSP,
                "Permissions-Policy" to PERMISSIONS_POLICY,
                "X-Content-Type-Options" to "nosniff"
            ),
            ByteArrayInputStream("Internal error".toByteArray())
        )
    }

    /**
     * Create an HTML error page.
     * Parameters are HTML-escaped to prevent XSS if they ever contain user input.
     */
    private fun createErrorPage(title: String, message: String): String {
        val safeTitle = title.escapeHtml()
        val safeMessage = message.escapeHtml()
        return """
            <!DOCTYPE html>
            <html>
            <head>
                <meta charset="UTF-8">
                <meta name="viewport" content="width=device-width, initial-scale=1.0">
                <title>$title</title>
                <style>
                    body {
                        font-family: -apple-system, BlinkMacSystemFont, sans-serif;
                        display: flex;
                        flex-direction: column;
                        align-items: center;
                        justify-content: center;
                        min-height: 100vh;
                        margin: 0;
                        padding: 20px;
                        background: #0a0a0a;
                        color: #e0e0e0;
                        text-align: center;
                    }
                    h1 {
                        font-size: 24px;
                        margin-bottom: 16px;
                        color: #ff6b6b;
                    }
                    p {
                        font-size: 16px;
                        color: #a0a0a0;
                        max-width: 400px;
                    }
                    button {
                        margin-top: 24px;
                        padding: 12px 24px;
                        font-size: 16px;
                        background: #333;
                        color: #fff;
                        border: none;
                        border-radius: 8px;
                        cursor: pointer;
                    }
                </style>
            </head>
            <body>
                <h1>$safeTitle</h1>
                <p>$safeMessage</p>
                <button onclick="if(window.__MINIAPP_IPC__)window.__MINIAPP_IPC__.closeMiniApp();">Close Mini App</button>
            </body>
            </html>
        """.trimIndent()
    }

    /**
     * HTML-escape a string to prevent XSS.
     */
    private fun String.escapeHtml(): String {
        return this
            .replace("&", "&amp;")
            .replace("<", "&lt;")
            .replace(">", "&gt;")
            .replace("\"", "&quot;")
            .replace("'", "&#39;")
    }

    // ========================================
    // JNI Native Methods
    // ========================================

    /**
     * Handle a request for a file from the Mini App package.
     *
     * @param miniappId The Mini App identifier
     * @param packagePath Path to the .xdc file
     * @param path The requested file path
     * @return WebResourceResponse or null if file not found
     */
    private external fun handleMiniAppRequest(
        miniappId: String,
        packagePath: String,
        path: String
    ): WebResourceResponse?

    private external fun getSelfAddrNative(): String?
    private external fun getSelfNameNative(): String?
}
