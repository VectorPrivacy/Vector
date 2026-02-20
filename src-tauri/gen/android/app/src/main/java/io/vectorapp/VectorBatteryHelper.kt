package io.vectorapp

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings

/**
 * Static utility methods for battery optimization and background service control.
 * Uses SharedPreferences ("vector_prefs") so BootReceiver and MainActivity can
 * read the preference before any native Rust code initializes.
 */
object VectorBatteryHelper {

    private const val PREFS_NAME = "vector_prefs"
    private const val KEY_BG_SERVICE_ENABLED = "background_service_enabled"
    private const val KEY_BG_SERVICE_PROMPTED = "background_service_prompted"

    @JvmStatic
    fun isIgnoringBatteryOptimizations(context: Context): Boolean {
        val pm = context.getSystemService(Context.POWER_SERVICE) as PowerManager
        return pm.isIgnoringBatteryOptimizations(context.packageName)
    }

    @JvmStatic
    fun requestBatteryOptimization(context: Context) {
        try {
            val intent = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS).apply {
                data = Uri.parse("package:${context.packageName}")
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            context.startActivity(intent)
        } catch (e: Exception) {
            // Some OEM ROMs (MIUI, ColorOS) strip or redirect this system Activity.
            // Fall back to the app's battery settings page.
            try {
                val fallback = Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS).apply {
                    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                }
                context.startActivity(fallback)
            } catch (_: Exception) {
                android.util.Log.w("VectorBatteryHelper", "No battery optimization Activity available")
            }
        }
    }

    @JvmStatic
    fun getBackgroundServiceEnabled(context: Context): Boolean {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        return prefs.getBoolean(KEY_BG_SERVICE_ENABLED, true)
    }

    @JvmStatic
    fun setBackgroundServiceEnabled(context: Context, enabled: Boolean) {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        prefs.edit().putBoolean(KEY_BG_SERVICE_ENABLED, enabled).apply()
    }

    @JvmStatic
    fun getBackgroundServicePrompted(context: Context): Boolean {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        return prefs.getBoolean(KEY_BG_SERVICE_PROMPTED, false)
    }

    @JvmStatic
    fun setBackgroundServicePrompted(context: Context) {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        prefs.edit().putBoolean(KEY_BG_SERVICE_PROMPTED, true).apply()
    }

    @JvmStatic
    fun startBackgroundService(context: Context) {
        val intent = Intent(context, VectorNotificationService::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            context.startForegroundService(intent)
        } else {
            context.startService(intent)
        }
    }

    @JvmStatic
    fun stopBackgroundService(context: Context) {
        val intent = Intent(context, VectorNotificationService::class.java)
        context.stopService(intent)
    }
}
