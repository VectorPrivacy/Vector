package io.vectorapp.gpui

import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.os.Handler
import android.os.Looper

/**
 * The phone's tilt for the badge card, as the Tauri client reads it: gravity (or the raw
 * accelerometer, on every phone) against the first reading, as pitch and roll in degrees.
 * Listens only while the card is open.
 */
object Tilt {
    private var manager: SensorManager? = null
    private var active = false
    private var baseline = false
    private var gx = 0.0
    private var gy = 0.0
    private var gz = 0.0
    private var baseX = 0.0
    private var baseY = 0.0
    private var lastMs = 0L
    private val main = Handler(Looper.getMainLooper())

    @JvmStatic external fun nativeTilt(rx: Float, ry: Float)

    private val listener = object : SensorEventListener {
        override fun onSensorChanged(event: SensorEvent) {
            val v = event.values
            if (!baseline) {
                gx = v[0].toDouble(); gy = v[1].toDouble(); gz = v[2].toDouble()
                val m = Math.sqrt(gx * gx + gy * gy + gz * gz)
                if (m < 1e-3) return
                baseX = gx / m; baseY = gy / m
                baseline = true
                return
            }
            // Low-pass: gravity without the hand's motion.
            val a = 0.15
            gx += (v[0] - gx) * a; gy += (v[1] - gy) * a; gz += (v[2] - gz) * a
            val m = Math.sqrt(gx * gx + gy * gy + gz * gz)
            if (m < 1e-3) return
            val now = System.currentTimeMillis()
            if (now - lastMs < 30) return
            lastMs = now
            var rx = -(gy / m - baseY) * 28.0
            var ry = (gx / m - baseX) * 28.0
            if (Math.abs(rx) < 0.5) rx = 0.0
            if (Math.abs(ry) < 0.5) ry = 0.0
            nativeTilt(rx.toFloat(), ry.toFloat())
        }

        override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) {}
    }

    /** False when the phone has no sensor for it: the pointer keeps the tilt. */
    @JvmStatic
    fun start(context: Context): Boolean {
        val sm = manager ?: (context.getSystemService(Context.SENSOR_SERVICE) as? SensorManager)?.also { manager = it }
            ?: return false
        val sensor = sm.getDefaultSensor(Sensor.TYPE_GRAVITY) ?: sm.getDefaultSensor(Sensor.TYPE_ACCELEROMETER)
            ?: return false
        main.post {
            if (!active) {
                baseline = false
                sm.registerListener(listener, sensor, SensorManager.SENSOR_DELAY_GAME, main)
                active = true
            }
        }
        return true
    }

    @JvmStatic
    fun stop() {
        main.post {
            if (active) {
                manager?.unregisterListener(listener)
                active = false
            }
        }
    }
}
