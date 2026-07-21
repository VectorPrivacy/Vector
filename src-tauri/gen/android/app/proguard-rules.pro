# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile

# ============================================
# Mini Apps (WebXDC) - JNI and JavaScript Interface
# ============================================

# Keep the JavaScript interface for Mini Apps WebView
-keepclassmembers class io.vectorapp.miniapp.MiniAppIpc {
    @android.webkit.JavascriptInterface <methods>;
}

# Keep every @JavascriptInterface method (called from JS, invisible to R8). Covers the
# Vector overlay bridge (__vectorOverlay), which lives on an anonymous class.
-keepclassmembers class * {
    @android.webkit.JavascriptInterface <methods>;
}

# Keep MiniAppIpc native JNI methods
-keepclassmembers class io.vectorapp.miniapp.MiniAppIpc {
    native <methods>;
}

# Keep MiniAppManager native JNI methods (callbacks to Rust)
-keepclassmembers class io.vectorapp.miniapp.MiniAppManager {
    native <methods>;
}

# Keep MiniAppManager companion object static methods (called from Rust via JNI)
-keep class io.vectorapp.miniapp.MiniAppManager {
    public static void initialize(android.app.Activity);
    public static void openMiniApp(java.lang.String, java.lang.String, java.lang.String, java.lang.String, java.lang.String, java.lang.String);
    public static void closeMiniApp();
    public static void sendToMiniApp(java.lang.String, java.lang.String);
    public static void sendRealtimeData(java.lang.String);
    public static java.lang.String pollRealtimeData();
    public static void closeMiniAppFromCrash();
    public static void handlePermissionResult(int, int[]);
    public static boolean isOpen();
    public static java.lang.String getCurrentMiniAppId();
    public static java.lang.String getCurrentPackagePath();
}

# Keep MiniAppWebViewClient native JNI method for file serving
-keepclassmembers class io.vectorapp.miniapp.MiniAppWebViewClient {
    native <methods>;
}

# ============================================
# Background Sync - JNI and Foreground Service
# ============================================

# Keep VectorNotificationService native JNI methods and static helpers
-keep class io.vectorapp.VectorNotificationService { *; }

# Keep BootReceiver (referenced in AndroidManifest)
-keep class io.vectorapp.BootReceiver { *; }

# Keep MainActivity native JNI methods (foreground state tracking)
-keepclassmembers class io.vectorapp.MainActivity {
    native <methods>;
}

# Keep NotificationActionReceiver JNI methods and constants (inline reply + mark as read)
-keep class io.vectorapp.NotificationActionReceiver { *; }

# Keep VectorBatteryHelper static methods (called from Rust via JNI)
-keep class io.vectorapp.VectorBatteryHelper { *; }

# Keep VectorFiles static methods (called from Rust via JNI: external media dir,
# MediaScanner, Open/Share intents). Without this, R8 strips/renames them since
# they're only referenced from native code — crashing release builds at boot
# (externalMediaDir is resolved during startup) with NoSuchMethodError.
-keep class io.vectorapp.VectorFiles { *; }

# Keep ExternalSigner (NIP-55 Amber bridge). Its static methods (isInstalled,
# queryContentResolver, launch, handleActivityResult) are invoked from Rust via
# JNI, and its native callback nativeOnSignerResult must keep its exact name so
# the Rust symbol resolves. R8 can't see JNI uses, so keep the whole class or
# release builds hit NoSuchMethodError the moment the login screen probes for a
# signer.
-keep class io.vectorapp.ExternalSigner { *; }