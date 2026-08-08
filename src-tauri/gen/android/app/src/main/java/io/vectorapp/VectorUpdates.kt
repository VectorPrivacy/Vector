package io.vectorapp

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import androidx.core.content.FileProvider
import java.io.File
import java.security.MessageDigest

/**
 * In-app update install, for SIDELOADED builds only.
 *
 * A store-installed copy is updated by its store; Rust decides that upstream
 * and never calls in here. What this adds over "open the file" is the check
 * below: Android refuses an update signed by a different key, and finding that
 * out AFTER a download is a miserable way to learn it.
 */
object VectorUpdates {
    /**
     * SHA-256 of the certificate that signed the installed app, lowercase hex.
     * Null if it cannot be read — callers treat that as "cannot verify".
     */
    @JvmStatic
    fun installedSignatureSha256(context: Context): String? = try {
        val pm = context.packageManager
        @Suppress("DEPRECATION")
        val flags = if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.P) {
            PackageManager.GET_SIGNING_CERTIFICATES
        } else {
            PackageManager.GET_SIGNATURES
        }
        @Suppress("DEPRECATION")
        val info = pm.getPackageInfo(context.packageName, flags)
        certDigest(signaturesOf(info))
    } catch (_: Throwable) {
        null
    }

    /**
     * SHA-256 of the certificate that signed the APK at `path`, lowercase hex.
     * Null if the file is unreadable or not a parseable package — which is
     * itself a reason to refuse, so callers must not treat null as a match.
     */
    @JvmStatic
    fun apkSignatureSha256(context: Context, path: String): String? = try {
        val pm = context.packageManager
        @Suppress("DEPRECATION")
        val flags = if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.P) {
            PackageManager.GET_SIGNING_CERTIFICATES
        } else {
            PackageManager.GET_SIGNATURES
        }
        @Suppress("DEPRECATION")
        val info = pm.getPackageArchiveInfo(path, flags)
        if (info == null) null else certDigest(signaturesOf(info))
    } catch (_: Throwable) {
        null
    }

    @Suppress("DEPRECATION")
    private fun signaturesOf(info: android.content.pm.PackageInfo): Array<android.content.pm.Signature>? {
        if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.P) {
            val signing = info.signingInfo ?: return null
            // A rotated key reports its history; the CURRENT signer is what an
            // update is judged against.
            return if (signing.hasMultipleSigners()) signing.apkContentsSigners
            else signing.signingCertificateHistory
        }
        return info.signatures
    }

    private fun certDigest(sigs: Array<android.content.pm.Signature>?): String? {
        val first = sigs?.firstOrNull() ?: return null
        val digest = MessageDigest.getInstance("SHA-256").digest(first.toByteArray())
        return digest.joinToString("") { "%02x".format(it) }
    }

    /**
     * Launch the system installer for a downloaded update.
     *
     * Refuses unless the APK is signed by the same certificate as the running
     * app: Android would reject the install anyway, and refusing here turns a
     * confusing platform error into something we can explain. Returns a status
     * the Rust side maps to a message.
     */
    @JvmStatic
    fun installUpdate(context: Context, path: String): String {
        val file = File(path)
        if (!file.exists()) return "missing"

        val installed = installedSignatureSha256(context) ?: return "unverifiable"
        val incoming = apkSignatureSha256(context, path) ?: return "unverifiable"
        if (installed != incoming) return "signature-mismatch"

        if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.O &&
            !context.packageManager.canRequestPackageInstalls()
        ) {
            val settings = Intent(
                android.provider.Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                Uri.parse("package:" + context.packageName)
            ).apply { addFlags(Intent.FLAG_ACTIVITY_NEW_TASK) }
            context.startActivity(settings)
            return "needs-permission"
        }

        val authority = context.packageName + ".fileprovider"
        val uri: Uri = FileProvider.getUriForFile(context, authority, file)
        val install = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, "application/vnd.android.package-archive")
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        context.startActivity(install)
        return "ok"
    }
}
