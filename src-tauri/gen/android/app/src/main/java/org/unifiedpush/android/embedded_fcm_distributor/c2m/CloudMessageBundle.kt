package org.unifiedpush.android.embedded_fcm_distributor.c2m

import android.os.Bundle

internal object CloudMessageBundle {
    private const val MSG_ID = "google.message_id"
    private const val PRODUCT_ID = "google.product_id"

    fun forMessage(message: Bundle): Bundle {
        val bundle = Bundle()
        message.getString(MSG_ID)?.let {
            bundle.putString(MSG_ID, it)
        }
        if (message.containsKey(PRODUCT_ID)) {
            bundle.putInt(PRODUCT_ID, message.getInt(PRODUCT_ID))
        }
        return bundle
    }
}
