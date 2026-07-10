package io.vectorapp.miniapp

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.RectF
import android.view.MotionEvent
import android.webkit.WebView

/**
 * Transparent WebView that hosts Vector's own overlay UI (overlay.html) on top of a
 * Mini App. It is the beginning of a cross-platform "Vector Overlay" — the same HTML
 * drives desktop later.
 *
 * The overlay is full-screen but mostly empty, so any touch that misses a live control
 * must fall through to the Mini App beneath. The JS layer reports the current control
 * bounds via the __vectorOverlay bridge; a DOWN outside those bounds is declined here,
 * which lets the sibling Mini App WebView below receive the gesture instead.
 */
@SuppressLint("ViewConstructor")
class OverlayWebView(context: Context) : WebView(context) {

    // Live control bounds in device px. Null until JS reports (touches pass through until then).
    @Volatile
    private var hitRect: RectF? = null

    fun setHitRectPx(rect: RectF?) {
        hitRect = rect
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_DOWN) {
            val r = hitRect
            if (r == null || !r.contains(event.x, event.y)) {
                return false // miss → let the Mini App below handle this gesture
            }
        }
        return super.onTouchEvent(event)
    }
}
