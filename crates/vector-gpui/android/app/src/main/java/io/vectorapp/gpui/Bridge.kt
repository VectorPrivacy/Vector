package io.vectorapp.gpui

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.view.Surface
import androidx.activity.OnBackPressedCallback
import io.vectorapp.MainActivity
import java.lang.ref.WeakReference

/**
 * The JNI boundary with GPUI (crates/vector-gpui/vendor/gpui_android/src/bridge.rs). Everything
 * `native*` hands an event to GPUI's thread and returns; the rest is called from that thread and
 * does its UI work on the main one.
 */
object Bridge {
    init {
        System.loadLibrary("vector_gpui")
    }

    private val main = Handler(Looper.getMainLooper())
    private var activity = WeakReference<MainActivity>(null)
    private lateinit var context: Context

    /** Back goes to GPUI only while it has somewhere to go back to. */
    val back = object : OnBackPressedCallback(false) {
        override fun handleOnBackPressed() = nativeBack()
    }

    fun attach(activity: MainActivity) {
        this.activity = WeakReference(activity)
        context = activity.applicationContext
        nativeStart(context)
    }

    private fun ui(action: (MainActivity) -> Unit) {
        main.post { activity.get()?.let(action) }
    }

    @JvmStatic external fun nativeStart(context: Context)
    @JvmStatic external fun nativeSurfaceCreated(surface: Surface)
    @JvmStatic external fun nativeSurfaceChanged(width: Int, height: Int)
    /** Returns once GPUI has stopped drawing into the surface. */
    @JvmStatic external fun nativeSurfaceDestroyed()
    /** Phase 0 down, 1 move, 2 up, 3 cancelled; `uptimeNanos` on SystemClock's uptime clock. */
    @JvmStatic external fun nativeTouch(id: Int, phase: Int, x: Float, y: Float, pressure: Float, uptimeNanos: Long)
    @JvmStatic external fun nativeKey(down: Boolean, keyCode: Int, unicode: Int, base: Int, meta: Int, repeat: Int)
    /** Pixels the system bars and cutouts cover, and the keyboard's height. */
    @JvmStatic external fun nativeInsets(top: Float, right: Float, bottom: Float, left: Float, ime: Float)
    @JvmStatic external fun nativeConfiguration(density: Float, dark: Boolean)
    /** 1 started, 2 resumed, 3 paused, 4 stopped, 5 destroyed. */
    @JvmStatic external fun nativeLifecycle(phase: Int)
    @JvmStatic external fun nativeFocus(focused: Boolean)
    @JvmStatic external fun nativeBack()
    @JvmStatic external fun nativeTrimMemory(level: Int)
    @JvmStatic external fun nativeOpenUrl(url: String)
    /** The keyboard's copy of the focused field after its edit number `serial`; -1 is no composing region. */
    @JvmStatic external fun nativeImeState(text: String, selStart: Int, selEnd: Int, compStart: Int, compEnd: Int, serial: Int)
    @JvmStatic external fun nativeImeAction(action: Int)

    @JvmStatic fun showKeyboard() = ui { it.view.showKeyboard() }

    @JvmStatic fun hideKeyboard() = ui { it.view.hideKeyboard() }

    @JvmStatic fun setBackEnabled(enabled: Boolean) {
        main.post { back.isEnabled = enabled }
    }

    @JvmStatic fun finish() = ui { it.finish() }

    @JvmStatic fun openUrl(url: String) = ui {
        try {
            it.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
        } catch (_: ActivityNotFoundException) {
        }
    }

    @JvmStatic fun setClipboard(text: String) {
        main.post {
            context.getSystemService(ClipboardManager::class.java)
                ?.setPrimaryClip(ClipData.newPlainText(null, text))
        }
    }

    @JvmStatic fun getClipboard(): String? {
        val clip = context.getSystemService(ClipboardManager::class.java)?.primaryClip ?: return null
        if (clip.itemCount == 0) return null
        return clip.getItemAt(0).coerceToText(context)?.toString()
    }

    /** GPUI's field, after the keyboard edits up to `serial`; -1 is no composing region. */
    @JvmStatic fun setTextState(text: String, selStart: Int, selEnd: Int, compStart: Int, compEnd: Int, serial: Int) =
        ui { it.view.setTextState(text, selStart, selEnd, compStart, compEnd, serial) }

    /** The focused field's keyboard, packed: bit 0 autocorrect, bit 1 suggestions, bits 2-3 caps, 4-7 action. */
    @JvmStatic fun setInputOptions(options: Int) = ui { it.view.setInputOptions(options) }
}
