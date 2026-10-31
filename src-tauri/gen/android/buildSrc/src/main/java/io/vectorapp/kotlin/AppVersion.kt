import java.io.File
import java.util.Properties
import org.gradle.api.GradleException
import org.gradle.api.Project

/** The app's version, read from Cargo.toml, and the Android version code it maps to. */
object AppVersion {
    fun name(cargoToml: File): String {
        if (!cargoToml.exists()) return "0.0.1"
        return cargoToml.readLines()
            .find { it.trim().startsWith("version") }
            ?.substringAfter("=")
            ?.trim()
            ?.trim('"')
            ?: "0.0.1"
    }

    // A semantic version as a version code (e.g. 0.4.2-1 -> 40201, 0.4.2 -> 40299).
    //
    // The low two digits rank a version's previews below the release they lead to,
    // which is what lets Android accept preview -> official as an upgrade. Without
    // that slot a `0.4.2-1` APK scores below 0.4.1 and the install is refused as a
    // downgrade. Codes only ever grow, so the x100 widening is safe against every
    // build already in the wild.
    fun code(cargoToml: File): Int {
        val version = name(cargoToml)
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
    fun checkMirror(cargoToml: File, mirror: File) {
        val props = Properties().apply { mirror.inputStream().use { load(it) } }
        val name = name(cargoToml)
        val code = code(cargoToml)
        if (props.getProperty("versionName") != name || props.getProperty("versionCode")?.toIntOrNull() != code) {
            throw GradleException(
                "app/version.properties is out of step with Cargo.toml: expected versionName=$name versionCode=$code"
            )
        }
    }
}

// F-Droid's build carries nothing of Google's: push there is UnifiedPush through a
// distributor app, with no FCM distributor embedded (scripts/fdroid-build.sh sets this).
fun Project.isFdroidBuild(): Boolean = providers.environmentVariable("VECTOR_FDROID").isPresent
