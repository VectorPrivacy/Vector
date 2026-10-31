package io.vectorapp

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager

/**
 * A call's sound as a phone call's: the communication mode, played to the earpiece, the
 * speaker or a headset, and the screen dark while the phone is at the ear. Rust says when a
 * call needs it and whether out loud; the user may pick another route meanwhile.
 */
object CallAudioRouter {
    const val OFF = 0
    const val EAR = 1
    const val LOUD = 2

    private const val EARPIECE = "earpiece"
    private const val SPEAKER = "speaker"
    private const val BLUETOOTH = "bluetooth"
    private const val HEADSET = "headset"

    init {
        System.loadLibrary("vector_lib")
    }

    /** `current` is empty once the call's audio is handed back. */
    @JvmStatic
    external fun nativeRouteChanged(current: String, available: String)

    private val lock = Any()
    private var mode = OFF
    private var picked: String? = null
    private var focus: AudioFocusRequest? = null
    private var proximity: PowerManager.WakeLock? = null
    private var devices: AudioDeviceCallback? = null
    private var app: Context? = null

    @JvmStatic
    fun apply(context: Context, next: Int) {
        synchronized(lock) {
            val am = context.getSystemService(AudioManager::class.java) ?: return
            val was = mode
            mode = next
            if (next == OFF) {
                if (was != OFF) stop(am)
                return
            }
            if (was == OFF) start(context.applicationContext, am)
            route(am)
        }
    }

    /** The user's pick; false if this phone has no such route now. */
    @JvmStatic
    fun setRoute(context: Context, route: String): Boolean {
        synchronized(lock) {
            if (mode == OFF) return false
            val am = context.getSystemService(AudioManager::class.java) ?: return false
            if (route !in available(am)) return false
            picked = route
            route(am)
            return true
        }
    }

    private fun start(context: Context, am: AudioManager) {
        app = context
        picked = null
        val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build()
            )
            .build()
        am.requestAudioFocus(request)
        focus = request
        am.mode = AudioManager.MODE_IN_COMMUNICATION
        val callback = object : AudioDeviceCallback() {
            override fun onAudioDevicesAdded(added: Array<out AudioDeviceInfo>) {
                synchronized(lock) {
                    if (mode == OFF) return
                    // A headset put on mid-call takes the call, as on any phone.
                    if (added.any { routeOf(it.type) == BLUETOOTH || routeOf(it.type) == HEADSET }) picked = null
                    route(am)
                }
            }

            override fun onAudioDevicesRemoved(removed: Array<out AudioDeviceInfo>) {
                synchronized(lock) {
                    if (mode == OFF) return
                    route(am)
                }
            }
        }
        am.registerAudioDeviceCallback(callback, Handler(Looper.getMainLooper()))
        devices = callback
    }

    @Suppress("DEPRECATION")
    private fun stop(am: AudioManager) {
        devices?.let { am.unregisterAudioDeviceCallback(it) }
        devices = null
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            am.clearCommunicationDevice()
        } else {
            am.isSpeakerphoneOn = false
            if (am.isBluetoothScoOn) {
                am.isBluetoothScoOn = false
                am.stopBluetoothSco()
            }
        }
        am.mode = AudioManager.MODE_NORMAL
        focus?.let { am.abandonAudioFocusRequest(it) }
        focus = null
        holdProximity(false)
        picked = null
        nativeRouteChanged("", "")
    }

    /** The user's pick while it lasts; else a headset, else the ear or the speaker as the call asks. */
    private fun route(am: AudioManager) {
        val routes = available(am)
        val current = picked?.takeIf { it in routes } ?: when {
            BLUETOOTH in routes -> BLUETOOTH
            HEADSET in routes -> HEADSET
            mode == LOUD || EARPIECE !in routes -> SPEAKER
            else -> EARPIECE
        }
        select(am, current)
        holdProximity(current == EARPIECE)
        nativeRouteChanged(current, routes.joinToString(","))
    }

    @Suppress("DEPRECATION")
    private fun select(am: AudioManager, route: String) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val device = am.availableCommunicationDevices.firstOrNull { routeOf(it.type) == route }
            if (device != null) am.setCommunicationDevice(device)
            return
        }
        am.isSpeakerphoneOn = route == SPEAKER
        val sco = route == BLUETOOTH
        if (sco != am.isBluetoothScoOn) {
            try {
                if (sco) am.startBluetoothSco() else am.stopBluetoothSco()
                am.isBluetoothScoOn = sco
            } catch (e: Exception) {
                android.util.Log.w("CallAudioRouter", "bluetooth: ${e.message}")
            }
        }
    }

    /** In the order a picker lists them. A plugged-in headset stands in for the earpiece. */
    private fun available(am: AudioManager): List<String> {
        val types = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            am.availableCommunicationDevices.map { it.type }
        } else {
            am.getDevices(AudioManager.GET_DEVICES_OUTPUTS).map { it.type }
        }
        val found = types.mapNotNull { routeOf(it) }.toSet()
        return listOf(EARPIECE, HEADSET, SPEAKER, BLUETOOTH)
            .filter { it in found }
            .filter { it != EARPIECE || HEADSET !in found }
    }

    private fun routeOf(type: Int): String? = when (type) {
        AudioDeviceInfo.TYPE_BUILTIN_EARPIECE -> EARPIECE
        AudioDeviceInfo.TYPE_BUILTIN_SPEAKER -> SPEAKER
        AudioDeviceInfo.TYPE_BLUETOOTH_SCO -> BLUETOOTH
        AudioDeviceInfo.TYPE_WIRED_HEADSET, AudioDeviceInfo.TYPE_WIRED_HEADPHONES, AudioDeviceInfo.TYPE_USB_HEADSET -> HEADSET
        else -> if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && type == AudioDeviceInfo.TYPE_BLE_HEADSET) BLUETOOTH else null
    }

    /** At the ear the screen goes dark, so a cheek can't press anything. */
    private fun holdProximity(on: Boolean) {
        val held = proximity
        if (!on) {
            if (held?.isHeld == true) held.release(PowerManager.RELEASE_FLAG_WAIT_FOR_NO_PROXIMITY)
            return
        }
        if (held?.isHeld == true) return
        val pm = app?.getSystemService(PowerManager::class.java) ?: return
        if (!pm.isWakeLockLevelSupported(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK)) return
        val lock = held ?: pm.newWakeLock(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK, "vector:call-proximity").also {
            it.setReferenceCounted(false)
            proximity = it
        }
        lock.acquire()
    }
}
