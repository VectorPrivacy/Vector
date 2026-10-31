package org.unifiedpush.android.embedded_fcm_distributor

interface Gateway {
    val vapid: String
    fun getEndpoint(token: String): String
}
