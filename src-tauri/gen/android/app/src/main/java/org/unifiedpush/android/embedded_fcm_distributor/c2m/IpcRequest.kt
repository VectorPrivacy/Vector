package org.unifiedpush.android.embedded_fcm_distributor.c2m

import android.content.Context
import android.os.Bundle
import android.os.Message
import android.os.Messenger

internal sealed class IpcRequest(
    @GmsOpcode.Code val what: Int,
    val payload: Bundle,
    val oneWay: Boolean
) {
    class OneWayRequest(@GmsOpcode.Code what: Int, payload: Bundle) : IpcRequest(what, payload, true)
    class TwoWayRequest(@GmsOpcode.Code what: Int, payload: Bundle) : IpcRequest(what, payload, false)

    fun message(context: Context, replyTo: Messenger): Message {
        val data = Bundle().apply {
            putBoolean("oneWay", oneWay)
            putString("pkg", context.packageName)
            putBundle("data", payload)
        }

        val msg = Message.obtain()
        msg.what = what
        msg.arg1 = 1
        msg.replyTo = replyTo
        msg.data = data

        return msg
    }
}
