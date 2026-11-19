package io.vectorapp.gpui

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import java.io.File
import java.util.UUID

/** Documents the picker chose, copied into the cache, where GPUI can read them by path. */
object Picked {
    /** Each in a folder of its own, under its own name, so two picks of `photo.jpg` never collide. */
    fun copy(context: Context, uris: List<Uri>): Array<String> {
        val root = File(context.cacheDir, "picked")
        return uris.mapNotNull { uri ->
            try {
                val dir = File(root, UUID.randomUUID().toString()).apply { mkdirs() }
                val file = File(dir, name(context, uri))
                val input = context.contentResolver.openInputStream(uri) ?: return@mapNotNull null
                input.use { source -> file.outputStream().use { source.copyTo(it) } }
                file.absolutePath
            } catch (e: Exception) {
                android.util.Log.w("Picked", "copy failed", e)
                null
            }
        }.toTypedArray()
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
