package org.unifiedpush.android.embedded_fcm_distributor

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ResolveInfo
import android.os.Build

internal object Utils {

    // UnifiedPush intents

    /** Send new UnifiedPush endpoint */
    fun sendNewEndpoint(
        context: Context,
        connectionToken: String,
        fcmToken: String,
        useGateway: Boolean
    ) {
        val intent = Intent().apply {
            `package` = context.packageName
            action = ACTION_NEW_ENDPOINT
            val endpoint = if (useGateway) {
                getEndpoint(context, fcmToken)
            } else {
                ENDPOINT.format(fcmToken)
            }
            putExtra(EXTRA_ENDPOINT, endpoint)
            putExtra(EXTRA_TOKEN, connectionToken)
        }
        context.sendBroadcast(intent)
    }

    /** Send UnifiedPush unregistration */
    fun sendUnregistered(context: Context, token: String) {
        val intent = Intent().apply {
            `package` = context.packageName
            action = ACTION_UNREGISTERED
            putExtra(EXTRA_TOKEN, token)
        }
        context.sendBroadcast(intent)
    }

    /** Send UnifiedPush registration failure */
    fun sendRegistrationFailed(
        context: Context,
        connectionToken: String,
        reason: FailedReason,
    ) {
        val intent = Intent().apply {
            `package` = context.packageName
            action = ACTION_REGISTRATION_FAILED
            putExtra(EXTRA_TOKEN, connectionToken)
            putExtra(EXTRA_FAILED_REASON, reason.name)
        }
        context.sendBroadcast(intent)
    }

    /** Send UnifiedPush Message */
    fun sendMessage(context: Context,
                    token: String,
                    message: ByteArray,
                    messageId: String?) {
        val intent = Intent().apply {
            action = ACTION_MESSAGE
            setPackage(context.packageName)
            putExtra(EXTRA_BYTES_MESSAGE, message)
            putExtra(EXTRA_TOKEN, token)
            messageId?.let {
                putExtra(EXTRA_MESSAGE_ID, messageId)
            }
        }
        context.sendBroadcast(intent)
    }

    // Shared Prefs

    fun saveEndpoint(context: Context, gateway: Gateway) {
        context.getSharedPreferences(PREF_MASTER, Context.MODE_PRIVATE)
            .edit()
            .putString(PREF_ENDPOINT, gateway.getEndpoint("%s"))
            .apply()
    }

    private fun getEndpoint(context: Context, token: String): String? {
        return context.getSharedPreferences(PREF_MASTER, Context.MODE_PRIVATE)
            .getString(PREF_ENDPOINT, null)
            ?.format(token)
    }

    fun saveVapid(context: Context, connectionToken: String, vapid: String) {
        context.getSharedPreferences(PREF_MASTER, Context.MODE_PRIVATE)
            .edit()
            .putString(PREF_VAPID.format(connectionToken), vapid)
            .apply()
    }

    fun getVapid(context: Context, connectionToken: String): String? {
        return context.getSharedPreferences(PREF_MASTER, Context.MODE_PRIVATE)
            .getString(PREF_VAPID.format(connectionToken), null)
    }
    // Other

    fun getResolveInfo(
        context: Context,
        action: String
    ): List<ResolveInfo> {
        val intent =
            Intent(action).apply {
                `package` = context.packageName
            }
        return (
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    context.packageManager.queryBroadcastReceivers(
                        intent,
                        PackageManager.ResolveInfoFlags.of(
                            PackageManager.GET_META_DATA.toLong() +
                                    PackageManager.GET_RESOLVED_FILTER.toLong(),
                        ),
                    )
                } else {
                    context.packageManager.queryBroadcastReceivers(
                        intent,
                        PackageManager.GET_RESOLVED_FILTER,
                    )
                }
                )
    }
}
