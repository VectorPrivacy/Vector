# Add project specific ProGuard rules here.
# By default, the flags in this file are appended to flags specified
# in an Android Library's build.gradle file.

# Keep the JNA classes and our generated bindings
-keep class com.sun.jna.** { *; }
-keep class * extends com.sun.jna.** { *; }
-keep class build.marmot.mdk.** { *; }
