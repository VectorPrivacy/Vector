package org.unifiedpush.android.embedded_fcm_distributor.c2m

import android.content.Context
import android.os.Bundle
import android.util.Log
import kotlinx.coroutines.flow.first
import org.unifiedpush.android.embedded_fcm_distributor.EXTRA_DELETE
import org.unifiedpush.android.embedded_fcm_distributor.EXTRA_KID
import org.unifiedpush.android.embedded_fcm_distributor.EXTRA_REGISTRATION_ID
import org.unifiedpush.android.embedded_fcm_distributor.EXTRA_SCOPE
import org.unifiedpush.android.embedded_fcm_distributor.EXTRA_SENDER
import org.unifiedpush.android.embedded_fcm_distributor.EXTRA_SUB
import org.unifiedpush.android.embedded_fcm_distributor.EXTRA_SUBTYPE
import org.unifiedpush.android.embedded_fcm_distributor.EXTRA_SUBTYPE_X
import org.unifiedpush.android.embedded_fcm_distributor.EXTRA_SUB_X
import org.unifiedpush.android.embedded_fcm_distributor.Utils

internal object C2mRequests {
    private val TAG = C2mRequests::class.java.simpleName

    /** Register to Google services */
    suspend fun c2mRegister(
        context: Context,
        channelId: String,
        vapid: String,
        useGateway: Boolean,
        onSuccess: (token: String) -> Unit,
        onFailure: () -> Unit,
    ) {
        val data = Bundle()
        setAttributesToBundle(channelId, vapid, false, useGateway, data)
        val request = IpcRequest.TwoWayRequest(GmsOpcode.SEND, data)
        val result = MessengerIpcClient.sendTwoWay(context, request).first().mapFcmToken()
        result.onSuccess { fcmToken ->
            Log.d(TAG, "Registration succeed")
            onSuccess(fcmToken)
        }
        result.onFailure { e ->
            Log.e(TAG, "Couldn't register.", e)
            onFailure()
        }
    }

    suspend fun c2mUnregister(
        context: Context,
        channelId: String
    ) {
        // We don't have the VAPID key from old registrations that
        // haven't been re-registered yet. We can simply ignore, that's ok
        val vapid = Utils.getVapid(context, channelId) ?: return
        val data = Bundle()
        setAttributesToBundle(channelId, vapid, true, null, data)
        val request = IpcRequest.TwoWayRequest(GmsOpcode.SEND, data)
        val result = MessengerIpcClient.sendTwoWay(context, request).first().mapFcmToken()
        result.onSuccess { fcmToken ->
            // fcmToken should be context.packageName
            Log.d(TAG, "Unregistration succeed: $fcmToken")
        }
        result.onFailure { e ->
            Log.e(TAG, "Couldn't unregister.", e)
        }
    }

    private fun Result<Bundle>.mapFcmToken(): Result<String> = this.mapCatching {
        // If Play Services can't access the internet, we get a SERVICE_NOT_AVAILABLE.
        // Another error would give INTERNAL_SERVER_ERROR or InternalServerError
        // cf. https://github.com/firebase/firebase-android-sdk/blob/30f4a1bcf392bcb8463c84f075f90b653751e412/firebase-messaging/src/main/java/com/google/firebase/messaging/GmsRpc.java#L343
        // But we already check for internet connection and return FailedReason.NETWORK if internet is inaccessible
        val regId = it.getString(EXTRA_REGISTRATION_ID) ?: throw Exception("Missing registration id: $it")
        regId.split(':', limit = 3).getOrNull(2) ?: throw Exception("Invalid registration Id")
    }

    /**
     * @param useGateway is null during unregistration
     */
    private fun setAttributesToBundle(
        channelId: String,
        vapid: String,
        delete: Boolean,
        useGateway: Boolean?,
        extras: Bundle
    ) {
        val subtype = "wp:$channelId"
        val kid = useGateway?.let {
            if (useGateway) "1:$channelId" else "0:$channelId"
        } ?: "2:$channelId"
        extras.putString(EXTRA_SCOPE, "GCM")
        // Compatibility with old GMS - Required for the moment, or we get a INVALID_PARAMETERS
        extras.putString(EXTRA_SENDER, vapid)
        // New name for the source
        extras.putString(EXTRA_SUB, vapid)
        extras.putString(EXTRA_SUB_X, vapid)
        extras.putString(EXTRA_SUBTYPE, subtype)
        extras.putString(EXTRA_SUBTYPE_X, subtype)
        if (delete) extras.putString(EXTRA_DELETE, "1")
        // When we receive a new token, the extra contains
        // "KID:TOKEN"
        extras.putString(EXTRA_KID, kid)
    }
}