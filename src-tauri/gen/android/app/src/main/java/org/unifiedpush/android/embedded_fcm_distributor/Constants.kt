package org.unifiedpush.android.embedded_fcm_distributor

/**
 * Constants as defined on the specs
 * https://unifiedpush.org/developers/spec/android/
 */

internal const val PREF_MASTER = "UP-embedded_fcm"
internal const val PREF_ENDPOINT = "$PREF_MASTER:endpoint"
internal const val PREF_VAPID = "$PREF_MASTER:vapid:%s"

internal const val ACTION_NEW_ENDPOINT = "org.unifiedpush.android.connector.NEW_ENDPOINT"
internal const val ACTION_REGISTRATION_FAILED = "org.unifiedpush.android.connector.REGISTRATION_FAILED"
internal const val ACTION_UNREGISTERED = "org.unifiedpush.android.connector.UNREGISTERED"
internal const val ACTION_MESSAGE = "org.unifiedpush.android.connector.MESSAGE"

internal const val ACTION_REGISTER = "org.unifiedpush.android.distributor.REGISTER"
internal const val ACTION_UNREGISTER = "org.unifiedpush.android.distributor.UNREGISTER"

internal const val EXTRA_PI = "pi"
internal const val EXTRA_TOKEN = "token"
internal const val EXTRA_VAPID = "vapid"
internal const val EXTRA_ENDPOINT = "endpoint"
internal const val EXTRA_FAILED_REASON = "reason"
internal const val EXTRA_BYTES_MESSAGE = "bytesMessage"
internal const val EXTRA_MESSAGE_ID = "id"


/*
 * FCM Related constants
 */
internal const val ENDPOINT = "https://fcm.googleapis.com/fcm/send/%s"

internal const val GSF_PACKAGE = "com.google.android.gms"
internal const val MIN_VERSION_FOR_MESSAGE_ACK = 233700000

internal const val ACTION_C2M_REGISTER = "com.google.android.c2dm.intent.REGISTER"
internal const val ACTION_C2M_REGISTRATION = "com.google.android.c2dm.intent.REGISTRATION"
internal const val ACTION_C2M_RECEIVE = "com.google.android.c2dm.intent.RECEIVE"

/**
 * Internal parameter used to indicate a 'subtype',
 * one subtype is a registration ~= instance
 */
internal const val EXTRA_SUBTYPE = "subtype"
internal const val EXTRA_SUBTYPE_X = "X-subtype"

/** Extra used to indicate which senders (Google API project IDs) can send messages to the app  */
internal const val EXTRA_SENDER = "sender"
internal const val EXTRA_SUB = "subscription"
internal const val EXTRA_SUB_X = "X-subscription"
internal const val EXTRA_SCOPE = "scope"
internal const val EXTRA_KID = "kid"
internal const val EXTRA_DELETE = "delete"

internal const val EXTRA_REGISTRATION_ID = "registration_id"
internal const val EXTRA_RAW_DATA = "rawData"
internal const val EXTRA_GOOGLE_MSG_ID = "google.message_id"
