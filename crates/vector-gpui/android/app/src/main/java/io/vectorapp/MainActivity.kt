package io.vectorapp

import android.content.Intent
import android.content.res.Configuration
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import io.vectorapp.gpui.Bridge
import io.vectorapp.gpui.GpuiView

/** The whole app: GPUI draws every pixel of [view] and hears the activity's life through [Bridge]. */
class MainActivity : ComponentActivity() {
    lateinit var view: GpuiView
        private set

    companion object {
        /** The ring's notification was tapped (`answer` false) or answered. */
        @JvmStatic
        external fun nativeOnCallTap(callId: String, npub: String, answer: Boolean)
    }

    /** The desktop client's glue (calls, permissions) holds this activity while it lives. */
    private external fun nativeCreated()
    private external fun nativeDestroyed()
    private external fun nativeForeground(on: Boolean)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        WindowCompat.setDecorFitsSystemWindows(window, false)
        view = GpuiView(this)
        setContentView(view)
        Bridge.attach(this)
        nativeCreated()
        configuration(resources.configuration)
        ViewCompat.setOnApplyWindowInsetsListener(view) { _, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime()).bottom
            Bridge.nativeInsets(bars.top.toFloat(), bars.right.toFloat(), bars.bottom.toFloat(), bars.left.toFloat(), ime.toFloat())
            WindowInsetsCompat.CONSUMED
        }
        onBackPressedDispatcher.addCallback(this, Bridge.back)
        link(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        link(intent)
    }

    private fun link(intent: Intent?) {
        intent ?: return
        val callNpub = intent.getStringExtra(IncomingCall.EXTRA_CALL_NPUB)
        if (!callNpub.isNullOrEmpty()) {
            val callId = intent.getStringExtra(IncomingCall.EXTRA_CALL_ID) ?: ""
            val answer = intent.getBooleanExtra(IncomingCall.EXTRA_CALL_ANSWER, false)
            intent.removeExtra(IncomingCall.EXTRA_CALL_NPUB)
            // A ringing call shows over the lock screen.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
                setShowWhenLocked(true)
                setTurnScreenOn(true)
            }
            nativeOnCallTap(callId, callNpub, answer)
            return
        }
        intent.takeIf { it.action == Intent.ACTION_VIEW }?.dataString?.let(Bridge::nativeOpenUrl)
    }

    private fun configuration(config: Configuration) {
        val dark = config.uiMode and Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
        Bridge.nativeConfiguration(resources.displayMetrics.density, dark)
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        configuration(newConfig)
    }

    override fun onStart() {
        super.onStart()
        Bridge.nativeLifecycle(1)
    }

    override fun onResume() {
        super.onResume()
        nativeForeground(true)
        Bridge.nativeLifecycle(2)
    }

    override fun onPause() {
        Bridge.nativeLifecycle(3)
        nativeForeground(false)
        super.onPause()
    }

    override fun onStop() {
        Bridge.nativeLifecycle(4)
        super.onStop()
    }

    override fun onDestroy() {
        Bridge.nativeLifecycle(5)
        nativeDestroyed()
        super.onDestroy()
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        PermissionHandler.handlePermissionResult(requestCode, grantResults)
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        Bridge.nativeFocus(hasFocus)
    }

    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        Bridge.nativeTrimMemory(level)
    }
}
