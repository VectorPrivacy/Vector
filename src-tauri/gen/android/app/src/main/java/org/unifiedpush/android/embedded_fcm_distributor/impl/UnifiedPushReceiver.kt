package org.unifiedpush.android.embedded_fcm_distributor.impl

import android.content.Context
import android.content.Intent
import org.unifiedpush.android.embedded_fcm_distributor.ACTION_REGISTER
import org.unifiedpush.android.embedded_fcm_distributor.EmbeddedDistributorReceiver
import org.unifiedpush.android.embedded_fcm_distributor.Utils.getResolveInfo

/**
 * Implements [EmbeddedDistributorReceiver], used only if the application
 * doesn't expose another receiver extending it.
 *
 * Thanks to this receiver, applications don't need to extend [EmbeddedDistributorReceiver]
 * if they use a VAPID key.
 */
class UnifiedPushReceiver: EmbeddedDistributorReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        if (shouldRun(context)) {
            super.onReceive(context, intent)
        }
    }

    /**
     * Run if the application doesn't implement [MessagingReceiver]
     *
     * Cache the result
     *
     * @return `true` if there isn't any implementation of [MessagingReceiver] with priority >-500
     */
    private fun shouldRun(context: Context): Boolean {
        return shouldRun ?: getResolveInfo(context, ACTION_REGISTER)
            .none {
                it.priority > -500
            }.also {
                shouldRun = it
            }
    }

    companion object {
        private var shouldRun: Boolean? = null
    }
}
