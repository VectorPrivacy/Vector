import java.util.Properties
import java.io.FileInputStream

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

// Read version from Cargo.toml
val cargoTomlFile = file("../../../Cargo.toml")
val cargoVersion = if (cargoTomlFile.exists()) {
    cargoTomlFile.readLines()
        .find { it.trim().startsWith("version") }
        ?.substringAfter("=")
        ?.trim()
        ?.trim('"')
        ?: "0.0.1"
} else {
    "0.0.1"
}

// Convert semantic version to version code (e.g. 0.4.2-1 -> 40201, 0.4.2 -> 40299).
//
// The low two digits rank a version's previews below the release they lead to,
// which is what lets Android accept preview -> official as an upgrade. Without
// that slot a `0.4.2-1` APK scores below 0.4.1 and the install is refused as a
// downgrade. Codes only ever grow, so the x100 widening is safe against every
// build already in the wild.
fun versionToCode(version: String): Int {
    val core = version.substringBefore('-').substringBefore('+')
    val parts = core.split(".").map { it.toIntOrNull() ?: 0 }
    val major = parts.getOrElse(0) { 0 }
    val minor = parts.getOrElse(1) { 0 }
    val patch = parts.getOrElse(2) { 0 }

    // Stable sorts above every preview of the same version.
    val preview = version.substringAfter('-', "").substringBefore('+')
    val slot = if (preview.isEmpty()) 99 else preview.toIntOrNull()
        ?.takeIf { it in 1..98 }
        // The MSI bundler already requires a numeric pre-release identifier;
        // failing here keeps Android from silently minting an unorderable code,
        // which is unfixable once users have installed it.
        ?: throw GradleException(
            "Version '$version': preview identifier must be a number from 1 to 98 (e.g. 0.4.2-1)"
        )

    return (major * 10000 + minor * 100 + patch) * 100 + slot
}

// version.properties mirrors the Cargo-derived pair for tools that can only
// read a flat file (F-Droid's update checker). A stale mirror fails the build.
val versionProperties = Properties().apply {
    file("version.properties").inputStream().use { load(it) }
}
if (versionProperties.getProperty("versionName") != cargoVersion ||
    versionProperties.getProperty("versionCode")?.toIntOrNull() != versionToCode(cargoVersion)
) {
    throw GradleException(
        "app/version.properties is out of step with Cargo.toml: expected " +
        "versionName=$cargoVersion versionCode=${versionToCode(cargoVersion)}"
    )
}

android {
    compileSdk = 34
    namespace = "io.vectorapp"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "io.vectorapp"
        minSdk = 26
        targetSdk = 34
        versionCode = versionToCode(cargoVersion)
        versionName = cargoVersion
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
    kotlinOptions {
        jvmTarget = "1.8"
    }
    buildFeatures {
        buildConfig = true
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.6.1")
    implementation("androidx.appcompat:appcompat:1.6.1")
    implementation("com.google.android.material:material:1.8.0")
    implementation("androidx.core:core-ktx:1.12.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = "tauri.build.gradle.kts")