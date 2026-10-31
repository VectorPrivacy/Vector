package org.unifiedpush.android.embedded_fcm_distributor.impl

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import org.unifiedpush.android.embedded_fcm_distributor.*
import org.unifiedpush.android.embedded_fcm_distributor.Utils.sendMessage
import org.unifiedpush.android.embedded_fcm_distributor.Utils.sendNewEndpoint
import org.unifiedpush.android.embedded_fcm_distributor.Utils.sendUnregistered
import org.unifiedpush.android.embedded_fcm_distributor.c2m.CloudMessageBundle
import org.unifiedpush.android.embedded_fcm_distributor.c2m.GmsOpcode
import org.unifiedpush.android.embedded_fcm_distributor.c2m.IpcRequest
import org.unifiedpush.android.embedded_fcm_distributor.c2m.MessengerIpcClient

/**
 * This receivers interacts with Google Services and receives FCM message. It is exposed by the library.
 */
class FirebaseReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        intent.extras?.let { msg ->
            CoroutineScope(Dispatchers.IO).launch {
                val data = CloudMessageBundle.forMessage(msg).apply {
                    putBoolean("supports_message_handled", true)
                }
                val res = MessengerIpcClient.sendOneWay(
                    context,
                    IpcRequest.OneWayRequest(GmsOpcode.BROADCAST_ACK, data)
                ).first()
                res.onSuccess { res -> if (!res) Log.w(TAG, "BROADCAST_ACK not acked") }
                res.onFailure { e -> Log.e(TAG, "Couldn't ACK broadcast", e) }
            }
        }
        when (intent.action) {
            ACTION_C2M_REGISTRATION -> {
                if (intent.hasExtra(EXTRA_REGISTRATION_ID)) {
                    val registration = intent.getStringExtra(EXTRA_REGISTRATION_ID)?.split(":", limit = 3) ?: return
                    if (registration.size != 3) {
                        Log.d(TAG, "Cannot retrieve registration info, aborting.")
                        return
                    }
                    val useGateway = registration[0] == "1"
                    val connectionToken = registration[1]
                    val fcmToken = registration[2]
                    if (fcmToken == context.packageName) {
                        sendUnregistered(context, connectionToken)
                    } else {
                        sendNewEndpoint(context, connectionToken, fcmToken, useGateway)
                    }
                    Log.i(TAG,"Successfully registered for FCM")
                } else {
                    Log.e(
                        TAG,
                        "FCM registration intent did not contain registration_id: $intent"
                    )
                }
            }
             ACTION_C2M_RECEIVE -> {
                 val token = intent.getStringExtra(EXTRA_SUBTYPE)?.let {
                     if (it.startsWith("wp:")) {
                         it.substring(3)
                     } else {
                         Log.d(TAG, "Received message for unknown subtype. Starting with ${it.substring(0,3)}")
                         null
                     }
                 } ?: return
                 val message = intent.getByteArrayExtra(EXTRA_RAW_DATA) ?: return
                 val messageId = intent.getStringExtra(EXTRA_GOOGLE_MSG_ID)
                 sendMessage(context, token, message, messageId)
                 intent.extras?.let { handleMessage(context, it) }
            }
        }
    }

    private fun handleMessage(context: Context, msg: Bundle) {
        if (gsfVersion(context) < MIN_VERSION_FOR_MESSAGE_ACK) return
        CoroutineScope(Dispatchers.IO).launch {
            val res = MessengerIpcClient.sendOneWay(
                context,
                IpcRequest.OneWayRequest(GmsOpcode.RPC_ACK, CloudMessageBundle.forMessage(msg))
            ).first()
            res.onSuccess { res -> if (!res) Log.w(TAG, "RPC_ACK not acked") }
            res.onFailure { e -> Log.e(TAG, "Couldn't ACK RPC", e) }
        }
    }

    private fun gsfVersion(context: Context): Int = gsfVersion
        ?: context
        .packageManager
        .getPackageInfo(GSF_PACKAGE, 0)
        .versionCode.also {
            gsfVersion = it
        }

    private companion object {
        private var gsfVersion: Int? = null
        private const val TAG = "FirebaseReceiver"
    }
}
