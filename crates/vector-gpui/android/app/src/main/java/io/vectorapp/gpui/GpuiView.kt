package io.vectorapp.gpui

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Rect
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.text.Selection
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.UnderlineSpan
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.accessibility.AccessibilityManager
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityNodeProvider
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager

/**
 * The surface GPUI draws into, and the keyboard's way into GPUI's text fields: the keyboard edits
 * [field], a copy of the focused field, and each edit goes to GPUI as the copy's new state.
 */
@SuppressLint("ViewConstructor")
class GpuiView(context: Context) : SurfaceView(context), SurfaceHolder.Callback {
    val field = SpannableStringBuilder()
    private val imm = context.getSystemService(InputMethodManager::class.java)
    private var options = 0
    private var serial = 0
    private var reported: List<Any>? = null
    private val composing = UnderlineSpan()
    private val access = context.getSystemService(AccessibilityManager::class.java)

    private val nodes = object : AccessibilityNodeProvider() {
        override fun createAccessibilityNodeInfo(virtualViewId: Int): AccessibilityNodeInfo? =
            Bridge.nativeA11yNode(this@GpuiView, virtualViewId)

        override fun findFocus(focus: Int): AccessibilityNodeInfo? = Bridge.nativeA11yFocus(this@GpuiView, focus)

        override fun performAction(virtualViewId: Int, action: Int, arguments: Bundle?) =
            Bridge.nativeA11yAction(this@GpuiView, virtualViewId, action, arguments)
    }

    init {
        holder.addCallback(this)
        isFocusable = true
        isFocusableInTouchMode = true
        access?.addAccessibilityStateChangeListener { on -> if (!on) Bridge.nativeA11yOff() }
        addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> claimEdges() }
    }

    /** The rail's left-edge pull (top, bottom) in this view's pixels, while one shows. */
    private var leftPull: Pair<Int, Int>? = null

    /** `top`..`bottom` in GPUI's pixels; none to release it. */
    fun claimLeft(span: Pair<Float, Float>?) {
        val density = resources.displayMetrics.density
        leftPull = span?.let { (top, bottom) -> (top * density).toInt() to (bottom * density).toInt() }
        claimEdges()
    }

    /**
     * Vector draws its own side swipes (the Communities rail, swipe to reply), so the system back
     * gesture gives way along both side edges as far as Android allows: 200dp of height a side. On
     * the left it sits on the rail's pull tab while one shows, else where a thumb swipes.
     */
    private fun claimEdges() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q || width == 0 || height == 0) return
        val density = resources.displayMetrics.density
        val band = (200 * density).toInt().coerceAtMost(height)
        val edge = (48 * density).toInt()
        val thumb = (height * 2 / 5 - band / 2).coerceIn(0, height - band)
        val left = leftPull?.let { (top, bottom) -> ((top + bottom) / 2 - band / 2).coerceIn(0, height - band) } ?: thumb
        systemGestureExclusionRects = listOf(
            Rect(0, left, edge, left + band),
            Rect(width - edge, thumb, width, thumb + band),
        )
    }

    override fun getAccessibilityNodeProvider(): AccessibilityNodeProvider = nodes

    /** Explore by touch arrives as hover; a mouse's hover, with no screen reader on, is not for it. */
    override fun dispatchHoverEvent(event: MotionEvent): Boolean {
        if (access?.isTouchExplorationEnabled == true && Bridge.nativeA11yHover(this, event.action, event.x, event.y)) {
            return true
        }
        return super.dispatchHoverEvent(event)
    }

    override fun surfaceCreated(holder: SurfaceHolder) = Bridge.nativeSurfaceCreated(holder.surface)

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) =
        Bridge.nativeSurfaceChanged(width, height)

    override fun surfaceDestroyed(holder: SurfaceHolder) = Bridge.nativeSurfaceDestroyed()

    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> touch(event, event.actionIndex, 0)
            MotionEvent.ACTION_MOVE -> for (i in 0 until event.pointerCount) {
                // The samples batched since the last frame, so velocity sees every one.
                for (h in 0 until event.historySize) {
                    Bridge.nativeTouch(
                        event.getPointerId(i), 1, event.getHistoricalX(i, h), event.getHistoricalY(i, h),
                        event.getHistoricalPressure(i, h), event.getHistoricalEventTime(h) * 1_000_000,
                    )
                }
                touch(event, i, 1)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> touch(event, event.actionIndex, 2)
            MotionEvent.ACTION_CANCEL -> for (i in 0 until event.pointerCount) touch(event, i, 3)
            else -> return false
        }
        return true
    }

    private fun touch(event: MotionEvent, index: Int, phase: Int) {
        val time = if (Build.VERSION.SDK_INT >= 34) event.eventTimeNanos else event.eventTime * 1_000_000
        Bridge.nativeTouch(event.getPointerId(index), phase, event.getX(index), event.getY(index), event.getPressure(index), time)
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean = key(true, event) || super.onKeyDown(keyCode, event)

    override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean = key(false, event) || super.onKeyUp(keyCode, event)

    private fun key(down: Boolean, event: KeyEvent): Boolean {
        if (event.isSystem) return false
        Bridge.nativeKey(down, event.keyCode, event.getUnicodeChar(event.metaState), event.getUnicodeChar(0), event.metaState, event.repeatCount)
        return true
    }

    override fun onCheckIsTextEditor() = true

    override fun onCreateInputConnection(info: EditorInfo): InputConnection {
        val action = options shr 4 and 0xF
        var type = InputType.TYPE_CLASS_TEXT
        if (options and 1 != 0) type = type or InputType.TYPE_TEXT_FLAG_AUTO_CORRECT
        if (options and 2 == 0) type = type or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        type = type or when (options shr 2 and 3) {
            1 -> InputType.TYPE_TEXT_FLAG_CAP_WORDS
            2 -> InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
            3 -> InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS
            else -> 0
        }
        // No action of its own: Enter is a key, and the field takes many lines.
        if (action <= 1) type = type or InputType.TYPE_TEXT_FLAG_MULTI_LINE
        info.inputType = type
        info.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_FLAG_NO_EXTRACT_UI or when (action) {
            2 -> EditorInfo.IME_ACTION_DONE
            3 -> EditorInfo.IME_ACTION_GO
            4 -> EditorInfo.IME_ACTION_NEXT
            5 -> EditorInfo.IME_ACTION_PREVIOUS
            6 -> EditorInfo.IME_ACTION_SEARCH
            7 -> EditorInfo.IME_ACTION_SEND
            else -> EditorInfo.IME_ACTION_NONE
        }
        info.initialSelStart = Selection.getSelectionStart(field)
        info.initialSelEnd = Selection.getSelectionEnd(field)
        return KeyboardConnection(this)
    }

    fun showKeyboard() {
        requestFocus()
        imm.showSoftInput(this, 0)
    }

    fun hideKeyboard() {
        imm.hideSoftInputFromWindow(windowToken, 0)
    }

    fun setInputOptions(options: Int) {
        if (this.options != options) {
            this.options = options
            imm.restartInput(this)
        }
    }

    /** GPUI's field. One that predates the keyboard's latest edit is skipped: GPUI answers that edit too. */
    fun setTextState(text: String, selStart: Int, selEnd: Int, compStart: Int, compEnd: Int, serial: Int) {
        if (serial < this.serial) return
        val replaced = field.toString() != text
        if (replaced) field.replace(0, field.length, text)
        Selection.setSelection(field, selStart.coerceIn(0, field.length), selEnd.coerceIn(0, field.length))
        BaseInputConnection.removeComposingSpans(field)
        if (compStart >= 0 && compEnd > compStart && compEnd <= field.length) {
            field.setSpan(composing, compStart, compEnd, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE or Spanned.SPAN_COMPOSING)
        }
        reported = state()
        // A keyboard mid-word holds its own idea of the text, which only a restart resets.
        if (replaced) imm.restartInput(this) else imm.updateSelection(this, selStart, selEnd, compStart, compEnd)
    }

    private fun state(): List<Any> = listOf(
        field.toString(),
        Selection.getSelectionStart(field),
        Selection.getSelectionEnd(field),
        BaseInputConnection.getComposingSpanStart(field),
        BaseInputConnection.getComposingSpanEnd(field),
    )

    /** After the keyboard's edit (or its batch of them), GPUI gets the field as it now stands. */
    fun edited() {
        val now = state()
        if (now == reported) return
        reported = now
        serial += 1
        Bridge.nativeImeState(now[0] as String, now[1] as Int, now[2] as Int, now[3] as Int, now[4] as Int, serial)
    }
}

/** The keyboard's editor over [GpuiView.field]: the platform's own, reporting each edit. */
class KeyboardConnection(private val view: GpuiView) : BaseInputConnection(view, true) {
    private var batch = 0

    override fun getEditable() = view.field

    override fun beginBatchEdit(): Boolean {
        batch += 1
        return true
    }

    override fun endBatchEdit(): Boolean {
        if (batch > 0) batch -= 1
        if (batch == 0) view.edited()
        return batch > 0
    }

    private fun <T> edit(result: T): T {
        if (batch == 0) view.edited()
        return result
    }

    override fun commitText(text: CharSequence?, newCursorPosition: Int) = edit(super.commitText(text, newCursorPosition))

    override fun setComposingText(text: CharSequence?, newCursorPosition: Int) =
        edit(super.setComposingText(text, newCursorPosition))

    override fun setComposingRegion(start: Int, end: Int) = edit(super.setComposingRegion(start, end))

    override fun finishComposingText() = edit(super.finishComposingText())

    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int) =
        edit(super.deleteSurroundingText(beforeLength, afterLength))

    override fun deleteSurroundingTextInCodePoints(beforeLength: Int, afterLength: Int) =
        edit(super.deleteSurroundingTextInCodePoints(beforeLength, afterLength))

    override fun setSelection(start: Int, end: Int) = edit(super.setSelection(start, end))

    override fun performEditorAction(actionCode: Int): Boolean {
        Bridge.nativeImeAction(actionCode)
        return true
    }
}
