package org.unifiedpush.android.embedded_fcm_distributor.c2m

import androidx.annotation.IntDef

internal class GmsOpcode {

    @Retention(AnnotationRetention.SOURCE)
    @IntDef(value = [SEND, BROADCAST_ACK, RPC_ACK, PROXY_RETAIN, PROXY_FETCH])
    annotation class Code

    companion object {
        const val SEND: Int = 1
        const val BROADCAST_ACK: Int = 2
        const val RPC_ACK: Int = 3
        const val PROXY_RETAIN: Int = 4
        const val PROXY_FETCH: Int = 5
    }
}
