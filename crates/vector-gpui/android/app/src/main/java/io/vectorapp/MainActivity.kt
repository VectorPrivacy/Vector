package io.vectorapp

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import io.vectorapp.gpui.Bridge
import io.vectorapp.gpui.GpuiView
import io.vectorapp.gpui.Picked

/** The whole app: GPUI draws every pixel of [view] and hears the activity's life through [Bridge]. */
class MainActivity : ComponentActivity() {
    lateinit var view: GpuiView
        private set

    private val NOTIFICATION_PERMISSION_REQUEST = 1001

    /** The picker GPUI has open, by the id its answer goes back with; 0 is none. */
    private var pickId = 0
    private val picker = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val id = pickId
        pickId = 0
        val data = result.data.takeIf { result.resultCode == RESULT_OK }
        val uris = buildList {
            data?.clipData?.let { clip -> for (i in 0 until clip.itemCount) add(clip.getItemAt(i).uri) }
            if (isEmpty()) data?.data?.let(::add)
        }
        if (uris.isEmpty()) {
            Bridge.nativePicked(id, null)
        } else {
            Thread { Bridge.nativePicked(id, Picked.copy(applicationContext, uris)) }.start()
        }
    }

    fun pickFiles(id: Int, multiple: Boolean, extensions: Array<String>) {
        // One picker at a time: one still open answers as cancelled.
        if (pickId != 0) Bridge.nativePicked(pickId, null)
        pickId = id
        val types = Picked.mimeTypes(extensions)
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = types.singleOrNull() ?: "*/*"
            if (types.size > 1) putExtra(Intent.EXTRA_MIME_TYPES, types.toTypedArray())
            putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multiple)
        }
        try {
            picker.launch(intent)
        } catch (_: android.content.ActivityNotFoundException) {
            pickId = 0
            Bridge.nativePicked(id, null)
        }
    }

    companion object {
        /** The ring's notification was tapped (`answer` false) or answered. */
        @JvmStatic
        external fun nativeOnCallTap(callId: String, npub: String, answer: Boolean)
        /** A message notification was tapped. */
        @JvmStatic
        external fun nativeOnNotificationTap(chatId: String)
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
        VectorNotificationService.createChannels(this)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), NOTIFICATION_PERMISSION_REQUEST)
        }
        if (VectorBatteryHelper.getBackgroundServiceEnabled(this)) VectorBatteryHelper.startBackgroundService(this)
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
        intent.getStringExtra("chat_id")?.takeIf { it.isNotEmpty() }?.let { chat ->
            intent.removeExtra("chat_id")
            nativeOnNotificationTap(chat)
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
        VectorNotificationService.clearAllMessageHistory()
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

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<String>, grantResults: IntArray) {
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
