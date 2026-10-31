package io.vectorapp;

import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;

import org.unifiedpush.android.connector.UnifiedPush;

import java.util.List;

/**
 * Turning push on and off. A UnifiedPush app the user has (ntfy, Sunup, ...) is used first;
 * without one, FCM's own Web Push endpoint through Play Services, by the distributor this app
 * embeds. Neither involves a Firebase project.
 */
public final class VectorPush {
    private static final String INSTANCE = "default";
    private static final String GSF_PACKAGE = "com.google.android.gms";

    private static volatile boolean asking = false;

    private VectorPush() {}

    /** Ask for an endpoint, which arrives at VectorPushService. False when nothing can give one. */
    public static boolean enable(Activity activity, String vapid) {
        String own = activity.getPackageName();
        List<String> distributors = UnifiedPush.getDistributors(activity);
        boolean external = false;
        for (String d : distributors) {
            if (!d.equals(own)) external = true;
        }
        String current = UnifiedPush.getAckDistributor(activity);
        if (current == null) {
            if (external) {
                for (String d : distributors) {
                    if (!d.equals(own)) {
                        UnifiedPush.saveDistributor(activity, d);
                        break;
                    }
                }
            } else if (playServices(activity)) {
                UnifiedPush.saveDistributor(activity, own);
            } else {
                return false;
            }
        }
        asking = true;
        UnifiedPush.register(activity, INSTANCE, "Vector", vapid);
        return true;
    }

    public static void disable(Context context) {
        asking = false;
        UnifiedPush.unregister(context, INSTANCE);
        UnifiedPush.removeDistributor(context);
    }

    /** `busy` while an endpoint is on its way, `off` when one can be asked for, else `unsupported`. */
    public static String status(Context context) {
        if (asking) return "busy";
        for (String d : UnifiedPush.getDistributors(context)) {
            if (!d.equals(context.getPackageName())) return "off";
        }
        return playServices(context) ? "off" : "unsupported";
    }

    /** The distributor answered, with an endpoint or a refusal. */
    static void settled() {
        asking = false;
    }

    private static boolean playServices(Context context) {
        try {
            context.getPackageManager().getPackageInfo(GSF_PACKAGE, 0);
            return true;
        } catch (PackageManager.NameNotFoundException e) {
            return false;
        }
    }
}
