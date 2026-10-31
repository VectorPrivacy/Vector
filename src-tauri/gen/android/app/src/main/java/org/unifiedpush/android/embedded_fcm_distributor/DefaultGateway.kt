package org.unifiedpush.android.embedded_fcm_distributor

/**
 * This is the gateway hosted on [unifiedpush.org](https://unifiedpush.org). Please host your own gateway if possible.
 *
 * It uses the "webpushfcm" rewrite proxy from [common-proxies](https://codeberg.org/UnifiedPush/common-proxies)
 */
object DefaultGateway: Gateway {
    override val vapid = "BHNcG_luRWfsMIh1z2YxTNWlWHSMMciR8C3R1fwCdahG2zrnc3DRUltqtohzsiSRyUWsP7euJMxZ6Agb8lxBcHk"

    override fun getEndpoint(token: String): String {
        return "https://fcm.distributor.unifiedpush.org/wpfcm?t=$token"
    }
}
