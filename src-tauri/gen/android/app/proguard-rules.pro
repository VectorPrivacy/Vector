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
    public static void openMiniApp(java.lang.String, java.lang.String, java.lang.String, java.lang.String, java.lang.String);
    public static void closeMiniApp();
    public static void sendToMiniApp(java.lang.String, java.lang.String);
    public static void sendRealtimeData(byte[]);
    public static boolean isOpen();
    public static java.lang.String getCurrentMiniAppId();
}

# Keep MiniAppWebViewClient native JNI method for file serving
-keepclassmembers class io.vectorapp.miniapp.MiniAppWebViewClient {
    native <methods>;
}