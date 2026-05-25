package io.vectorapp

import android.content.Context
import android.content.Intent
import android.media.MediaScannerConnection
import android.net.Uri
import androidx.core.content.FileProvider
import java.io.File

/**
 * Native file helpers bridged from Rust via JNI static calls.
 *
 * Intent / FileProvider / MediaScanner work lives here in type-checked Kotlin
 * rather than raw JNI. Each method takes the Context as its first argument so
 * the helper holds no static state.
 */
object VectorFiles {
    /**
     * Vector's public download directory under app-specific external *media*
     * storage: /storage/emulated/0/Android/media/<pkg>/Vector.
     *
     * Chosen over /Android/data/<pkg>/ because the media path stays browsable
     * in file managers (Google hid /Android/data on Android 11+) and is
     * eligible for gallery indexing — all with no runtime permission. Returns
     * the absolute path, or null if external media storage is unavailable.
     */
    @JvmStatic
    fun externalMediaDir(context: Context): String? {
        val base = context.externalMediaDirs.firstOrNull { it != null } ?: return null
        val dir = File(base, "Vector")
        if (!dir.exists()) dir.mkdirs()
        return dir.absolutePath
    }

    /**
     * Ask the system MediaScanner to index a file so it shows up in the gallery
     * and file managers promptly instead of after the next full scan.
     */
    @JvmStatic
    fun scanFile(context: Context, path: String) {
        try {
            MediaScannerConnection.scanFile(context, arrayOf(path), null, null)
        } catch (_: Throwable) {
            // Best-effort: the file is still on disk and browsable regardless.
        }
    }

    /**
     * Batch variant of [scanFile] — indexes many files in a single scanner
     * request. Used by the migration to avoid per-file JNI + connection churn
     * for users with thousands of files.
     */
    @JvmStatic
    fun scanFiles(context: Context, paths: Array<String>) {
        try {
            if (paths.isNotEmpty()) {
                MediaScannerConnection.scanFile(context, paths, null, null)
            }
        } catch (_: Throwable) {
        }
    }

    /**
     * Open a file with the user's chosen app via an ACTION_VIEW chooser.
     * Hands out a content:// URI through the app's FileProvider with a
     * temporary read grant. Returns true if an activity was launched.
     */
    @JvmStatic
    fun openFile(context: Context, path: String): Boolean {
        return try {
            val file = File(path)
            if (!file.exists()) return false
            val authority = context.packageName + ".fileprovider"
            val uri: Uri = FileProvider.getUriForFile(context, authority, file)
            val mime = context.contentResolver.getType(uri) ?: "*/*"
            val view = Intent(Intent.ACTION_VIEW).apply {
                setDataAndType(uri, mime)
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            val chooser = Intent.createChooser(view, "Open with").apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            context.startActivity(chooser)
            true
        } catch (_: Throwable) {
            false
        }
    }

    /**
     * Share a file through Android's share sheet (ACTION_SEND), handing out a
     * content:// URI via the FileProvider with a temporary read grant. Returns
     * true if the share sheet was launched.
     */
    @JvmStatic
    fun shareFile(context: Context, path: String): Boolean {
        return try {
            val file = File(path)
            if (!file.exists()) return false
            val authority = context.packageName + ".fileprovider"
            val uri: Uri = FileProvider.getUriForFile(context, authority, file)
            val mime = context.contentResolver.getType(uri) ?: "*/*"
            val send = Intent(Intent.ACTION_SEND).apply {
                type = mime
                putExtra(Intent.EXTRA_STREAM, uri)
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            val chooser = Intent.createChooser(send, "Share").apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            context.startActivity(chooser)
            true
        } catch (_: Throwable) {
            false
        }
    }
}
