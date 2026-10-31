import java.util.Properties
import java.io.FileInputStream
import com.android.build.api.dsl.ApplicationExtension
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

// A stale version.properties (F-Droid's update checker reads it) fails the build.
AppVersion.checkMirror(file("../../../Cargo.toml"), file("version.properties"))

configure<ApplicationExtension> {
    compileSdk = 36
    namespace = "io.vectorapp"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "io.vectorapp"
        minSdk = 26
        targetSdk = 34
        versionCode = AppVersion.code(file("../../../Cargo.toml"))
        versionName = AppVersion.name(file("../../../Cargo.toml"))
        buildConfigField("boolean", "EMBEDDED_FCM", (!isFdroidBuild()).toString())
    }
    // Signing is optional: F-Droid builds unsigned and signs on its own
    // servers, so a missing keystore still yields a release APK.
    val keystorePropertiesFile = rootProject.file("keystore.properties")
    signingConfigs {
        if (keystorePropertiesFile.exists()) {
            create("release") {
                val keystoreProperties = Properties()
                keystoreProperties.load(FileInputStream(keystorePropertiesFile))
                keyAlias = keystoreProperties["keyAlias"] as String
                keyPassword = keystoreProperties["password"] as String
                storeFile = file(keystoreProperties["storeFile"] as String)
                storePassword = keystoreProperties["password"] as String
            }
        }
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            if (keystorePropertiesFile.exists()) {
                signingConfig = signingConfigs.getByName("release")
            }
            isMinifyEnabled = true
            proguardFiles(
                *fileTree(".") { include("**/*.pro") }
                    .plus(getDefaultProguardFile("proguard-android-optimize.txt"))
                    .toList().toTypedArray()
            )
        }
    }
    // Per-ABI APK splits: each device downloads only its architecture's native
    // libs (~half the size of the all-ABI universal). universalApk keeps one
    // works-everywhere fallback for manual sideloading. All splits share the
    // Cargo-derived versionCode — fine for direct/store distribution (a device
    // only installs the ABI that matches it; updates come from the next
    // release's higher code). Google Play would additionally need per-ABI codes.
    splits {
        abi {
            isEnable = true
            reset()
            include("armeabi-v7a", "arm64-v8a")
            isUniversalApk = true
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    buildFeatures {
        buildConfig = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    // 1.18: Profile.setCrossOriginIsolatedAllowlist, threaded Mini Apps (MiniAppIsolation.kt).
    implementation("androidx.webkit:webkit:1.18.0-alpha01")
    implementation("androidx.appcompat:appcompat:1.6.1")
    implementation("com.google.android.material:material:1.8.0")
    implementation("androidx.core:core-ktx:1.12.0")
    // Push: UnifiedPush in every build; FCM's endpoint, through Play Services, for phones with
    // no distributor app, everywhere but F-Droid.
    implementation("org.unifiedpush.android:connector:3.0.10")
    if (!isFdroidBuild()) {
        implementation("org.unifiedpush.android:embedded-fcm-distributor:3.1.0")
    }
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}