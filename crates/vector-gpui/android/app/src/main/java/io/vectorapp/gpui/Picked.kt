package io.vectorapp.gpui

import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.ParcelFileDescriptor
import android.system.Os
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import java.io.File
import java.io.FileOutputStream
import java.util.UUID

/** Documents the picker chose, where GPUI can read them by path: held in memory, or copied into the cache. */
object Picked {
    /** Past this a document is copied to the cache: one that large is a file on disk already. */
    private const val HELD_MAX = 256L * 1024 * 1024

    private class TooLarge : Exception()

    /** Each in a folder of its own, so two picks of `photo.jpg` never collide. */
    fun copy(context: Context, uris: List<Uri>): Array<String> {
        val root = File(context.cacheDir, "picked")
        return uris.mapNotNull { uri ->
            try {
                val dir = File(root, UUID.randomUUID().toString()).apply { mkdirs() }
                val name = name(context, uri)
                held(context, uri, dir, name) ?: run {
                    val file = File(dir, name)
                    val input = context.contentResolver.openInputStream(uri) ?: return@mapNotNull null
                    input.use { source -> file.outputStream().use { source.copyTo(it) } }
                    file.absolutePath
                }
            } catch (e: Exception) {
                android.util.Log.w("Picked", "copy failed", e)
                null
            }
        }.toTypedArray()
    }

    /**
     * The document in a memfd named after it, reached by a link that holds only the memfd's number:
     * a picture another app holds only in memory never touches disk. Null to copy it instead.
     */
    private fun held(context: Context, uri: Uri, dir: File, name: String): String? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return null
        val size = context.contentResolver.query(uri, arrayOf(OpenableColumns.SIZE), null, null, null)
            ?.use { if (it.moveToFirst() && !it.isNull(0)) it.getLong(0) else -1L } ?: -1L
        if (size > HELD_MAX) return null
        val fd = try {
            Os.memfd_create(name, 0)
        } catch (_: Exception) {
            try { Os.memfd_create("file", 0) } catch (_: Exception) { return null }
        }
        var kept = -1
        try {
            val input = context.contentResolver.openInputStream(uri) ?: return null
            // Not closed: closing the stream would close the memfd.
            val out = FileOutputStream(fd)
            input.use { source ->
                val buf = ByteArray(1 shl 16)
                var total = 0L
                while (true) {
                    val n = source.read(buf)
                    if (n < 0) break
                    total += n
                    if (total > HELD_MAX) throw TooLarge()
                    out.write(buf, 0, n)
                }
            }
            kept = ParcelFileDescriptor.dup(fd).detachFd()
            val ext = name.substringAfterLast('.', "")
            val link = File(dir, if (ext.isEmpty()) "file" else "file.$ext")
            Os.symlink("/proc/self/fd/$kept", link.path)
            return link.absolutePath
        } catch (e: Exception) {
            if (e !is TooLarge) android.util.Log.w("Picked", "not held", e)
            if (kept >= 0) ParcelFileDescriptor.adoptFd(kept).close()
            return null
        } finally {
            Os.close(fd)
        }
    }

    /** The document's own name, with no folder in it, and an extension from its type when it has none. */
    private fun name(context: Context, uri: Uri): String {
        val shown = context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { if (it.moveToFirst()) it.getString(0) else null }
        val base = shown?.substringAfterLast('/')?.trimStart('.')?.takeIf { it.isNotEmpty() } ?: "file"
        if (base.contains('.')) return base
        val ext = MimeTypeMap.getSingleton().getExtensionFromMimeType(context.contentResolver.getType(uri))
        return if (ext.isNullOrEmpty()) base else "$base.$ext"
    }

    /** The picker's types for `extensions`; any file when one has no known type. */
    fun mimeTypes(extensions: Array<String>): List<String> {
        val map = MimeTypeMap.getSingleton()
        val types = extensions.map { map.getMimeTypeFromExtension(it.lowercase()) }
        return if (types.isEmpty() || types.any { it == null }) listOf("*/*") else types.filterNotNull().distinct()
    }
}
