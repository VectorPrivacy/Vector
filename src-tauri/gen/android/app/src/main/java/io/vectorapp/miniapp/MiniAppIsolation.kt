package io.vectorapp.miniapp

import android.webkit.WebView
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature
import io.vectorapp.Logger

/**
 * Cross-origin isolation (SharedArrayBuffer, threaded WebAssembly) for Mini Apps that opt in.
 * WebView has no COOP/COEP isolation; it isolates an origin its profile allowlists when the
 * page sends Document-Isolation-Policy (WebView 153+), which miniapp_jni.rs does for these apps.
 */
internal object MiniAppIsolation {
    private const val TAG = "MiniAppIsolation"

    /** Allowlists [origin] on [webView]'s profile before it loads; false where WebView can't. */
    fun allow(webView: WebView, origin: String): Boolean {
        if (!WebViewFeature.isFeatureSupported(WebViewFeature.CROSS_ORIGIN_ISOLATED_ALLOWLIST) ||
            !WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE)
        ) {
            return false
        }
        return try {
            val profile = WebViewCompat.getProfile(webView)
            val origins = HashSet(profile.crossOriginIsolatedAllowlist)
            if (origins.add(origin)) profile.setCrossOriginIsolatedAllowlist(origins)
            true
        } catch (e: Exception) {
            Logger.error(TAG, "Could not allowlist $origin for cross-origin isolation", e)
            false
        }
    }
}
