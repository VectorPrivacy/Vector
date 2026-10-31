package io.vectorapp;

import android.util.Log;

import org.unifiedpush.android.connector.FailedReason;
import org.unifiedpush.android.connector.PushService;
import org.unifiedpush.android.connector.data.PushEndpoint;
import org.unifiedpush.android.connector.data.PushMessage;

/**
 * UnifiedPush's events for this app (a distributor app's, or the embedded FCM one's), handed to
 * Rust (`android/push.rs`). Java rather than Kotlin: the connector is built by a newer Kotlin
 * than this project's, whose metadata a Java caller never reads.
 */
public class VectorPushService extends PushService {
    static {
        System.loadLibrary("vector_lib");
    }

    private static native void nativeNewEndpoint(Object context, String dataDir, String url, String p256dh, String auth);

    private static native void nativeMessage(Object context, String dataDir, byte[] content);

    private static native void nativeUnregistered(Object context, String dataDir);

    private String dataDir() {
        return getApplicationContext().getDataDir().getAbsolutePath();
    }

    @Override
    public void onNewEndpoint(PushEndpoint endpoint, String instance) {
        // Without Web Push keys a sender can't seal for this phone.
        VectorPush.settled();
        if (endpoint.getPubKeySet() == null) {
            Log.w("VectorPush", "endpoint without Web Push keys");
            return;
        }
        nativeNewEndpoint(getApplicationContext(), dataDir(), endpoint.getUrl(),
                endpoint.getPubKeySet().getPubKey(), endpoint.getPubKeySet().getAuth());
    }

    @Override
    public void onMessage(PushMessage message, String instance) {
        // Sealed pushes are always encrypted: one that isn't came from somewhere else.
        if (!message.getDecrypted()) return;
        nativeMessage(getApplicationContext(), dataDir(), message.getContent());
    }

    @Override
    public void onRegistrationFailed(FailedReason reason, String instance) {
        Log.w("VectorPush", "registration failed: " + reason);
        VectorPush.settled();
    }

    @Override
    public void onUnregistered(String instance) {
        nativeUnregistered(getApplicationContext(), dataDir());
    }
}
