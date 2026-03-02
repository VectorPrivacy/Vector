package io.vectorapp.miniapp

import android.webkit.JavascriptInterface
import io.vectorapp.Logger


/**
 * JavaScript interface for Mini App IPC communication.
 *
 * This class provides the bridge between JavaScript running in the Mini App
 * WebView and the Rust backend via JNI.
 *
 * Methods annotated with @JavascriptInterface are callable from JavaScript
 * via window.__MINIAPP_IPC__.methodName()
 */
class MiniAppIpc(
    private val miniappId: String,
    private val packagePath: String
) {
    companion object {
        private const val TAG = "MiniAppIpc"

        /**
         * Maximum size for realtime channel data (128 KB).
         * This matches the WebXDC specification limit.
         */
        private const val REALTIME_DATA_MAX_SIZE = 128_000

        init {
            System.loadLibrary("vector_lib")
        }

        /**
         * Check granted permissions without needing a JavascriptInterface context.
         * Used by MiniAppChromeClient to gate native permission requests.
         */
        fun getGrantedPermissionsStatic(miniappId: String, packagePath: String): String? {
            return MiniAppIpc(miniappId, packagePath).getGrantedPermissions().ifEmpty { null }
        }
    }

    /**
     * Invoke a Mini App command and return the result.
     *
     * @param command The command name (e.g., "miniapp_get_updates")
     * @param args JSON-encoded arguments
     * @return JSON-encoded result, or null on error
     */
    @JavascriptInterface
    fun invokeCommand(command: String, args: String): String? {
        Logger.debug(TAG, "[$miniappId] invokeCommand: $command")
        return try {
            invokeNative(miniappId, packagePath, command, args)
        } catch (e: Exception) {
            Logger.error(TAG, "[$miniappId] invokeCommand failed: ${e.message}", null)
            """{"error":"${e.message?.replace("\"", "\\\"")}"}"""
        }
    }

    /**
     * Send a state update from the Mini App.
     *
     * @param update JSON-encoded update payload
     * @param description Human-readable description of the update
     */
    @JavascriptInterface
    fun sendUpdate(update: String, description: String) {
        Logger.debug(TAG, "[$miniappId] sendUpdate: $description")
        try {
            sendUpdateNative(miniappId, update, description)
        } catch (e: Exception) {
            Logger.error(TAG, "[$miniappId] sendUpdate failed: ${e.message}", null)
        }
    }

    /**
     * Get updates since a given serial number.
     *
     * @param lastKnownSerial The last serial number the app has seen
     * @return JSON array of updates
     */
    @JavascriptInterface
    fun getUpdates(lastKnownSerial: Int): String {
        Logger.debug(TAG, "[$miniappId] getUpdates since: $lastKnownSerial")
        return try {
            getUpdatesNative(miniappId, lastKnownSerial) ?: "[]"
        } catch (e: Exception) {
            Logger.error(TAG, "[$miniappId] getUpdates failed: ${e.message}", null)
            "[]"
        }
    }

    /**
     * Join the realtime channel for multiplayer functionality.
     *
     * @return Topic ID for the channel, or null on error
     */
    @JavascriptInterface
    fun joinRealtimeChannel(): String? {
        Logger.info(TAG, "[$miniappId] joinRealtimeChannel")
        return try {
            joinRealtimeChannelNative(miniappId)
        } catch (e: Exception) {
            Logger.error(TAG, "[$miniappId] joinRealtimeChannel failed: ${e.message}", null)
            null
        }
    }

    /**
     * Send data through the realtime channel.
     *
     * @param dataEncoded Base91-encoded binary data (decoded in Rust)
     */
    @JavascriptInterface
    fun sendRealtimeData(dataEncoded: String) {
        Logger.debug(TAG, "[$miniappId] sendRealtimeData: ${dataEncoded.length} chars")
        try {
            sendRealtimeDataNative(miniappId, dataEncoded)
        } catch (e: Exception) {
            Logger.error(TAG, "[$miniappId] sendRealtimeData failed: ${e.message}", null)
        }
    }

    /**
     * Leave the realtime channel.
     */
    @JavascriptInterface
    fun leaveRealtimeChannel() {
        Logger.info(TAG, "[$miniappId] leaveRealtimeChannel")
        try {
            leaveRealtimeChannelNative(miniappId)
        } catch (e: Exception) {
            Logger.error(TAG, "[$miniappId] leaveRealtimeChannel failed: ${e.message}", null)
        }
    }

    /**
     * Get the user's self address (npub).
     *
     * @return The user's npub string
     */
    @JavascriptInterface
    fun getSelfAddr(): String {
        return try {
            getSelfAddrNative() ?: "unknown"
        } catch (e: Exception) {
            Logger.error(TAG, "[$miniappId] getSelfAddr failed: ${e.message}", null)
            "unknown"
        }
    }

    /**
     * Get the user's display name.
     *
     * @return The user's display name
     */
    @JavascriptInterface
    fun getSelfName(): String {
        return try {
            getSelfNameNative() ?: "Unknown"
        } catch (e: Exception) {
            Logger.error(TAG, "[$miniappId] getSelfName failed: ${e.message}", null)
            "Unknown"
        }
    }

    /**
     * Get granted permissions for this Mini App.
     *
     * @return Comma-separated list of granted permission names
     */
    @JavascriptInterface
    fun getGrantedPermissions(): String {
        return try {
            getGrantedPermissionsNative(miniappId, packagePath) ?: ""
        } catch (e: Exception) {
            Logger.error(TAG, "[$miniappId] getGrantedPermissions failed: ${e.message}", null)
            ""
        }
    }

    /**
     * Poll queued realtime data from the native side.
     * Returns a JSON array of base91-encoded strings, or empty string if none.
     * Called from JS on a background thread (JavascriptInterface runs on WebKit thread).
     */
    @JavascriptInterface
    fun pollRealtimeData(): String {
        return try {
            MiniAppManager.pollRealtimeData() ?: ""
        } catch (e: Exception) {
            Logger.error(TAG, "[$miniappId] pollRealtimeData failed: ${e.message}", null)
            ""
        }
    }

    /**
     * Close the Mini App.
     */
    @JavascriptInterface
    fun closeMiniApp() {
        Logger.info(TAG, "[$miniappId] closeMiniApp requested from JS")
        MiniAppManager.closeMiniApp()
    }

    // ========================================
    // JNI Native Methods
    // ========================================

    private external fun invokeNative(
        miniappId: String,
        packagePath: String,
        command: String,
        args: String
    ): String?

    private external fun sendUpdateNative(
        miniappId: String,
        update: String,
        description: String
    )

    private external fun getUpdatesNative(
        miniappId: String,
        lastKnownSerial: Int
    ): String?

    private external fun joinRealtimeChannelNative(miniappId: String): String?

    private external fun sendRealtimeDataNative(miniappId: String, data: String)

    private external fun leaveRealtimeChannelNative(miniappId: String)

    private external fun getSelfAddrNative(): String?

    private external fun getSelfNameNative(): String?

    private external fun getGrantedPermissionsNative(
        miniappId: String,
        packagePath: String
    ): String?
}
