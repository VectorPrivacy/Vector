package io.vectorapp

import android.content.Context
import org.json.JSONObject

/**
 * The shell's own text in the app's language: notifications, their channels and the system
 * prompts. The app sends it whenever the language is set; it is kept so a service or receiver
 * running before the app speaks the same language. English until the first send.
 */
object Strings {
    private const val PREFS = "vector_strings"
    private const val TABLE = "table"

    @Volatile private var app: Context? = null
    @Volatile private var table: Map<String, String>? = null

    /** Called from each entry point with a context, before it builds any text. */
    @JvmStatic
    fun init(context: Context) {
        if (app == null) app = context.applicationContext
    }

    /** From the app: `json` maps each message id to its text. */
    @JvmStatic
    fun set(context: Context, json: String) {
        init(context)
        val parsed = parse(json) ?: return
        if (parsed == table) return
        table = parsed
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putString(TABLE, json).apply()
        // A channel keeps its sound and importance, but takes a new name and description.
        VectorNotificationService.createChannels(context)
        IncomingCall.channels(context)
        CallService.channel(context)
    }

    @JvmStatic
    fun t(id: String, english: String): String {
        val known = table ?: app?.let { load(it) }?.also { table = it }
        return known?.get(id)?.takeIf { it.isNotEmpty() } ?: english
    }

    private fun load(context: Context): Map<String, String>? =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(TABLE, null)?.let { parse(it) }

    private fun parse(json: String): Map<String, String>? = try {
        val o = JSONObject(json)
        o.keys().asSequence().associateWith { o.optString(it) }
    } catch (_: Exception) {
        null
    }
}
