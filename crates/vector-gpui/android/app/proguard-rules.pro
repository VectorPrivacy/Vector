# The bridge is called by name from Rust over JNI.
-keep class io.vectorapp.gpui.Bridge { *; }
# The desktop client's call and permission glue, called by name from the same Rust.
-keep class io.vectorapp.CallService { *; }
-keep class io.vectorapp.IncomingCall { *; }
-keep class io.vectorapp.CallAudioRouter { *; }
-keep class io.vectorapp.PermissionHandler { *; }
-keepclassmembers class io.vectorapp.MainActivity {
    native <methods>;
}
-keep class io.vectorapp.NotificationActionReceiver { *; }
-keep class io.vectorapp.VectorNotificationService { *; }
-keep class io.vectorapp.VectorBatteryHelper { *; }
-keep class io.vectorapp.Strings { *; }
-keep class io.vectorapp.VectorFiles { *; }
-keep class io.vectorapp.BiometricUnlock { *; }
-keep class io.vectorapp.ExternalSigner { *; }
-keep class io.vectorapp.VectorUpdates { *; }
-keep class io.vectorapp.VectorPush { *; }
-keep class io.vectorapp.VectorPushService { *; }
