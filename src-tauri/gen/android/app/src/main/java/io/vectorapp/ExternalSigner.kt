package io.vectorapp

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.net.Uri
import org.json.JSONObject

/**
 * NIP-55 external-signer (Amber) bridge. Two transports:
 *
 *  - ContentResolver ([queryContentResolver]) — the silent background hot path
 *    for pre-authorized sign/encrypt/decrypt ops. No Activity, no UI.
 *  - Intent for result ([launch] + [handleActivityResult]) — pairing and the
 *    fallback when a ContentResolver query comes back not-pre-authorized.
 *
 * The native side ([nativeOnSignerResult]) wakes a blocking Rust waiter keyed
 * by the request id we echo through the intent.
 */
object ExternalSigner {
    const val REQUEST_CODE = 7801

    init {
        System.loadLibrary("vector_lib")
    }

    /** Rust-side callback: delivers an intent result JSON to the waiter for [requestId]. */
    @JvmStatic
    external fun nativeOnSignerResult(requestId: Int, resultJson: String)

    /**
     * Launch a signer intent for result. Called from Rust (JNI) on the UI Activity.
     * Empty-string args are treated as absent. `get_public_key` pairing passes an
     * empty [pkg] so the OS resolves the installed signer; every other op pins it.
     */
    @JvmStatic
    fun launch(
        activity: Activity,
        requestId: Int,
        type: String,
        data: String,
        pubkey: String,
        currentUser: String,
        permsJson: String,
        pkg: String
    ) {
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse("nostrsigner:" + data))
        intent.putExtra("type", type)
        if (pkg.isNotEmpty()) intent.setPackage(pkg)
        // Set both casings: Amethyst/Amber use camelCase `pubKey`, the NIP-55
        // prose says `pubkey`; Amber reads either, so send both for max compat.
        if (pubkey.isNotEmpty()) {
            intent.putExtra("pubKey", pubkey)
            intent.putExtra("pubkey", pubkey)
        }
        if (currentUser.isNotEmpty()) intent.putExtra("current_user", currentUser)
        if (permsJson.isNotEmpty()) intent.putExtra("permissions", permsJson)
        // Echo the request id so the result can be routed back to its waiter.
        intent.putExtra("id", requestId.toString())
        activity.startActivityForResult(intent, REQUEST_CODE)
    }

    /**
     * Forwarded from MainActivity.onActivityResult. Packages the returned extras
     * into a JSON blob and wakes the native waiter. A cancelled result (user
     * declined / back) is reported as `rejected`.
     */
    @JvmStatic
    fun handleActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (requestCode != REQUEST_CODE) return
        val requestId = data?.getStringExtra("id")?.toIntOrNull() ?: -1
        val obj = JSONObject()
        if (resultCode != Activity.RESULT_OK || data == null) {
            obj.put("rejected", true)
        } else {
            data.getStringExtra("result")?.let { obj.put("result", it) }
            data.getStringExtra("event")?.let { obj.put("event", it) }
            data.getStringExtra("package")?.let { obj.put("package", it) }
            if (data.getBooleanExtra("rejected", false)) obj.put("rejected", true)
        }
        try {
            nativeOnSignerResult(requestId, obj.toString())
        } catch (_: Throwable) {}
    }

    /**
     * ContentResolver query for one op. `selection`/`selectionArgs`/`sortOrder`
     * are null; the data goes in the `projection` slot (Amber overloads it as
     * `[payload, counterparty, current_user]`). Returns a small JSON blob — a
     * TRI-STATE the caller must not collapse:
     *   {"result":...,"event":...}   success (read result / event)
     *   {"requires_approval":true}   null OR empty cursor = not remembered;
     *                                escalate to the foreground Intent. NOT a
     *                                rejection, NOT an empty decrypt.
     *   {"rejected":true}            a `rejected` column = a remembered reject;
     *                                hard stop, do NOT relaunch the signer.
     *   {"error":"..."}              IPC/exception failure.
     */
    @JvmStatic
    fun queryContentResolver(
        context: Context,
        authority: String,
        arg0: String,
        arg1: String,
        arg2: String
    ): String {
        return try {
            val uri = Uri.parse("content://$authority")
            val projection = arrayOf(arg0, arg1, arg2)
            val cursor = context.contentResolver.query(uri, projection, null, null, null)
                ?: return "{\"requires_approval\":true}"
            cursor.use { c ->
                if (!c.moveToFirst()) return "{\"requires_approval\":true}"
                // A `rejected` column is Amber's remembered-reject signal.
                if (c.getColumnIndex("rejected") >= 0) return "{\"rejected\":true}"
                val obj = JSONObject()
                val resIdx = c.getColumnIndex("result")
                if (resIdx >= 0 && !c.isNull(resIdx)) {
                    val s = c.getString(resIdx)
                    if (!s.isNullOrEmpty()) obj.put("result", s)
                }
                val evIdx = c.getColumnIndex("event")
                if (evIdx >= 0 && !c.isNull(evIdx)) {
                    val s = c.getString(evIdx)
                    if (!s.isNullOrEmpty()) obj.put("event", s)
                }
                obj.toString()
            }
        } catch (e: Exception) {
            val msg = (e.message ?: "content resolver error").replace("\"", "'")
            "{\"error\":\"$msg\"}"
        }
    }

    /** Whether any app can handle the `nostrsigner:` scheme. */
    @JvmStatic
    fun isInstalled(context: Context): Boolean {
        return try {
            val intent = Intent(Intent.ACTION_VIEW, Uri.parse("nostrsigner:"))
            context.packageManager.queryIntentActivities(intent, 0).isNotEmpty()
        } catch (_: Exception) {
            false
        }
    }
}
