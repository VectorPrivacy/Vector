import javax.inject.Inject
import org.gradle.process.ExecOperations

plugins {
    id("com.android.application")
}

val crateDir = rootProject.file("..")
val cargoToml = File(crateDir, "Cargo.toml")

/** The client's version, from its Cargo.toml. */
fun versionName(): String = cargoToml.readLines()
    .first { it.trim().startsWith("version") }
    .substringAfter("=").trim().trim('"')

/** The Tauri client's mapping, so either client updates the other: 0.6.1-2 is 60102, 0.6.1 is 60199. */
fun versionCode(): Int {
    val version = versionName()
    val parts = version.substringBefore('-').substringBefore('+').split(".").map { it.toIntOrNull() ?: 0 }
    val preview = version.substringAfter('-', "").substringBefore('+')
    val slot = if (preview.isEmpty()) 99 else preview.toIntOrNull()?.takeIf { it in 1..98 }
        ?: throw GradleException("Version '$version': a preview is a number from 1 to 98 (e.g. 0.6.0-1)")
    return (parts.getOrElse(0) { 0 } * 10000 + parts.getOrElse(1) { 0 } * 100 + parts.getOrElse(2) { 0 }) * 100 + slot
}

android {
    namespace = "io.vectorapp"
    compileSdk = 36
    ndkVersion = "29.0.14206865"
    defaultConfig {
        // The Tauri client's id: an install updates in place and keeps its data.
        applicationId = "io.vectorapp"
        minSdk = 26
        targetSdk = 34
        versionCode = versionCode()
        versionName = versionName()
    }
    val keystore = rootProject.file("keystore.properties")
    signingConfigs {
        if (keystore.exists()) {
            create("release") {
                val props = java.util.Properties().apply { keystore.inputStream().use { load(it) } }
                keyAlias = props["keyAlias"] as String
                keyPassword = props["password"] as String
                storeFile = file(props["storeFile"] as String)
                storePassword = props["password"] as String
            }
        }
    }
    buildTypes {
        getByName("debug") {
            // Beside an installed Vector rather than over it, with data of its own.
            applicationIdSuffix = ".gpui"
            isDebuggable = true
            isJniDebuggable = true
        }
        getByName("release") {
            if (keystore.exists()) signingConfig = signingConfigs.getByName("release")
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

/** Builds the client as a cdylib for each ABI with the NDK, into a jniLibs tree. */
abstract class CargoBuild @Inject constructor(private val exec: ExecOperations) : DefaultTask() {
    @get:Input abstract val abis: ListProperty<String>
    @get:Input abstract val profile: Property<String>
    @get:Input abstract val features: Property<String>
    @get:Internal abstract val crateDir: DirectoryProperty
    @get:Internal abstract val ndkDir: DirectoryProperty
    @get:OutputDirectory abstract val jniLibs: DirectoryProperty

    init {
        // Cargo knows what is stale; Gradle never skips it.
        outputs.upToDateWhen { false }
    }

    @TaskAction
    fun build() {
        val os = System.getProperty("os.name").lowercase()
        val host = when {
            os.contains("mac") -> "darwin-x86_64"
            os.contains("windows") -> "windows-x86_64"
            else -> "linux-x86_64"
        }
        val ext = if (os.contains("windows")) ".cmd" else ""
        val ndk = ndkDir.get().asFile
        val bin = File(ndk, "toolchains/llvm/prebuilt/$host/bin")
        val sysroot = File(ndk, "toolchains/llvm/prebuilt/$host/sysroot/usr/lib")
        val crate = crateDir.get().asFile
        val outDir = if (profile.get() == "dev") "debug" else profile.get()
        for (abi in abis.get()) {
            val (triple, clang, libDir) = when (abi) {
                "arm64-v8a" -> Triple("aarch64-linux-android", "aarch64-linux-android", "aarch64-linux-android")
                "armeabi-v7a" -> Triple("armv7-linux-androideabi", "armv7a-linux-androideabi", "arm-linux-androideabi")
                "x86_64" -> Triple("x86_64-linux-android", "x86_64-linux-android", "x86_64-linux-android")
                else -> throw GradleException("No Rust target for ABI $abi")
            }
            val key = triple.replace('-', '_')
            exec.exec {
                workingDir = crate
                commandLine(
                    "cargo", "rustc", "--lib", "--crate-type", "cdylib", "--locked",
                    "--target", triple, "--profile", profile.get(),
                    "--no-default-features", "--features", features.get(),
                )
                environment("ANDROID_NDK_HOME", ndk.path)
                environment("ANDROID_NDK_ROOT", ndk.path)
                environment("ANDROID_NDK", ndk.path)
                environment("CC_$key", File(bin, "${clang}26-clang$ext").path)
                environment("CXX_$key", File(bin, "${clang}26-clang++$ext").path)
                environment("AR_$key", File(bin, "llvm-ar").path)
                environment("CARGO_TARGET_${key.uppercase()}_LINKER", File(bin, "${clang}26-clang$ext").path)
            }
            val dest = jniLibs.get().dir(abi).asFile.apply { mkdirs() }
            val lib = File(crate, "target/$triple/$outDir/libvector_gpui.so")
            lib.copyTo(File(dest, lib.name), overwrite = true)
            // C++ built through the cc crate links the shared runtime, which the APK must carry.
            val cxx = File(dest, "libc++_shared.so")
            if (lib.readBytes().containsAscii("libc++_shared.so")) {
                File(sysroot, "$libDir/libc++_shared.so").copyTo(cxx, overwrite = true)
            } else {
                cxx.delete()
            }
        }
    }

    private fun ByteArray.containsAscii(needle: String): Boolean {
        val n = needle.toByteArray()
        outer@ for (i in 0..size - n.size) {
            for (j in n.indices) if (this[i + j] != n[j]) continue@outer
            return true
        }
        return false
    }
}

// `-PvectorAbis=arm64-v8a,armeabi-v7a,x86_64`; a phone is arm64 unless told otherwise.
val abis = (findProperty("vectorAbis") as String? ?: "arm64-v8a").split(',').map { it.trim() }
val features = findProperty("vectorFeatures") as String? ?: "tor,i2p"

androidComponents {
    onVariants { variant ->
        val name = variant.name.replaceFirstChar { it.uppercase() }
        val cargo = tasks.register<CargoBuild>("cargoBuild$name") {
            this.abis.set(abis)
            // Debug APKs still run optimised: the unoptimised client is too slow on a phone.
            profile.set(if (variant.buildType == "release") "release" else "fastrel")
            this.features.set(features)
            crateDir.set(rootProject.layout.projectDirectory.dir(".."))
            ndkDir.set(androidComponents.sdkComponents.ndkDirectory)
            jniLibs.set(layout.buildDirectory.dir("rustJniLibs/${variant.name}"))
        }
        variant.sources.jniLibs?.addGeneratedSourceDirectory(cargo, CargoBuild::jniLibs)
    }
}

dependencies {
    implementation("androidx.activity:activity:1.9.3")
    implementation("androidx.core:core:1.13.1")
}
