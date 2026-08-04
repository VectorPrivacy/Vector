package io.vectorapp

import android.app.Activity
import android.content.Context
import android.hardware.biometrics.BiometricManager
import android.hardware.biometrics.BiometricPrompt
import android.os.Build
import android.os.CancellationSignal
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import org.json.JSONObject
import java.security.InvalidKeyException
import java.security.KeyStore
import java.security.UnrecoverableKeyException
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Biometric unlock bridge. Wraps Vector's 32-byte Local Encryption vault key
 * with an auth-gated AES-256-GCM key in the Android Keystore (StrongBox when
 * available, TEE otherwise):
 *
 *  - [enroll] — generate a fresh keystore key and encrypt the vault key behind
 *    one BiometricPrompt (the prompt doubles as proof the sensor works).
 *  - [unlock] — decrypt the wrapped blob behind a prompt; the plaintext bytes
 *    go straight back to Rust via [nativeOnBiometricKey] (never through a
 *    Java String/JSON) and into the GuardedKey vault.
 *
 * Android 11+ (API 30) only: `setAllowedAuthenticators(BIOMETRIC_STRONG |
 * DEVICE_CREDENTIAL)` needs 30, and gating there removes the whole weak-vs-
 * strong `canAuthenticate()` ambiguity of 29 and the negative-button rules of
 * pre-30 prompts. Framework BiometricPrompt (not androidx) — MainActivity is a
 * TauriActivity, not a FragmentActivity, and the framework API needs neither a
 * dependency nor a fragment host. The native callbacks wake a blocking Rust
 * waiter keyed by request id, mirroring the ExternalSigner bridge.
 */
object BiometricUnlock {

    init {
        System.loadLibrary("vector_lib")
    }

    /** Rust-side callback: status results (cancelled / invalidated / error / wrapped blob). */
    @JvmStatic
    external fun nativeOnBiometricResult(requestId: Int, resultJson: String)

    /** Rust-side callback: the unwrapped vault-key bytes (unlock success only). */
    @JvmStatic
    external fun nativeOnBiometricKey(requestId: Int, data: ByteArray)

    private const val KEYSTORE = "AndroidKeyStore"
    private const val GCM_TAG_BITS = 128

    /** Availability: "available", "none_enrolled" (no screen lock) or "unsupported". */
    @JvmStatic
    fun availability(context: Context): String {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return "unsupported"
        return try {
            // A secure lockscreen is the whole requirement on R+: the prompt's
            // DEVICE_CREDENTIAL path works with or without biometric sensors,
            // and auth-gated keystore keys demand a lockscreen regardless.
            // (BiometricManager.canAuthenticate with the combined mask reports
            // NO_HARDWARE on sensorless devices/emulators even though the
            // credential path is fully functional — it is NOT the truth here.)
            val km = context.getSystemService(android.app.KeyguardManager::class.java)
                ?: return "unsupported"
            if (km.isDeviceSecure) "available" else "none_enrolled"
        } catch (_: Exception) {
            "unsupported"
        }
    }

    /**
     * The OS's own localized wording for our authenticator combo ("Use
     * fingerprint", "Use screen lock", ...). Android never reveals the
     * credential TYPE (PIN vs pattern), but 12+ provides the right words via
     * BiometricManager.getStrings; 11 falls back to a biometric-vs-lock split.
     * Empty string = caller uses its default copy.
     */
    @JvmStatic
    fun unlockLabel(context: Context): String {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return ""
        return try {
            val bm = context.getSystemService(BiometricManager::class.java) ?: return ""
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                val label = bm.getStrings(
                    BiometricManager.Authenticators.BIOMETRIC_STRONG
                        or BiometricManager.Authenticators.DEVICE_CREDENTIAL
                )?.buttonLabel
                if (!label.isNullOrEmpty()) return label.toString()
            }
            if (bm.canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_STRONG)
                == BiometricManager.BIOMETRIC_SUCCESS
            ) "Use biometrics" else "Use screen lock"
        } catch (_: Exception) {
            ""
        }
    }

    /**
     * Enroll: replace any key under [alias] with a fresh auth-gated one and
     * encrypt [keyBytes] behind one prompt. Result: {"data": b64(iv||ct)} — the
     * wrapped blob is not secret, so the JSON channel is fine for the OUTPUT.
     * The key arrives as a ByteArray (never a String): JVM Strings are
     * immutable and cannot be wiped, so a String parameter would strand a
     * plaintext key copy in the heap until GC. Zeroed here and by the caller.
     */
    @JvmStatic
    fun enroll(activity: Activity, requestId: Int, alias: String, keyBytes: ByteArray) {
        activity.runOnUiThread {
            try {
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
                    keyBytes.fill(0)
                    finish(requestId, JSONObject().put("error", "requires Android 11+").toString())
                    return@runOnUiThread
                }
                val ks = KeyStore.getInstance(KEYSTORE).apply { load(null) }
                ks.deleteEntry(alias)
                val secret = generateKey(alias)
                val cipher = Cipher.getInstance("AES/GCM/NoPadding")
                // Keystore generates the IV (randomized encryption stays at its
                // required default) — never supply one on encrypt.
                cipher.init(Cipher.ENCRYPT_MODE, secret)
                prompt(activity, requestId, cipher, "Enable Biometric Unlock") { c ->
                    val ct = c.doFinal(keyBytes)
                    keyBytes.fill(0)
                    val iv = c.iv
                    val out = ByteArray(iv.size + ct.size)
                    iv.copyInto(out)
                    ct.copyInto(out, iv.size)
                    finish(requestId, JSONObject().put("data", Base64.encodeToString(out, Base64.NO_WRAP)).toString())
                }
            } catch (e: Exception) {
                keyBytes.fill(0)
                finish(requestId, err(e))
            }
        }
    }

    /**
     * Unlock: decrypt [wrappedB64] (12-byte IV || ct) under [alias] behind one
     * prompt; success delivers raw bytes via [nativeOnBiometricKey]. ANY
     * key-load/init/decrypt failure reports {"invalidated": true}: a keystore
     * key invalidated by biometric/lockscreen changes throws at Cipher.init
     * (before any prompt), a DB restored onto a new device finds no matching
     * key, and OEM variants surface as InvalidKey/UnrecoverableKey/BadTag —
     * all of them mean "this enrollment is dead, fall back to PIN", never a
     * retry loop.
     */
    @JvmStatic
    fun unlock(activity: Activity, requestId: Int, alias: String, wrappedB64: String) {
        activity.runOnUiThread {
            try {
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
                    finish(requestId, JSONObject().put("error", "requires Android 11+").toString())
                    return@runOnUiThread
                }
                val ks = KeyStore.getInstance(KEYSTORE).apply { load(null) }
                val secret = ks.getKey(alias, null) as? SecretKey
                if (secret == null) {
                    finish(requestId, JSONObject().put("invalidated", true).toString())
                    return@runOnUiThread
                }
                val blob = Base64.decode(wrappedB64, Base64.NO_WRAP)
                if (blob.size <= 12) {
                    finish(requestId, JSONObject().put("invalidated", true).toString())
                    return@runOnUiThread
                }
                val iv = blob.copyOfRange(0, 12)
                val ct = blob.copyOfRange(12, blob.size)
                val cipher = Cipher.getInstance("AES/GCM/NoPadding")
                cipher.init(Cipher.DECRYPT_MODE, secret, GCMParameterSpec(GCM_TAG_BITS, iv))
                prompt(activity, requestId, cipher, "Unlock Vector") { c ->
                    val plain = c.doFinal(ct)
                    try {
                        nativeOnBiometricKey(requestId, plain)
                    } catch (_: Throwable) {
                    }
                    plain.fill(0)
                }
            } catch (e: InvalidKeyException) {
                // Covers KeyPermanentlyInvalidatedException (its subclass) and
                // every other unusable-key shape.
                finish(requestId, JSONObject().put("invalidated", true).toString())
            } catch (e: UnrecoverableKeyException) {
                finish(requestId, JSONObject().put("invalidated", true).toString())
            } catch (e: Exception) {
                finish(requestId, err(e))
            }
        }
    }

    /** Delete the keystore key under [alias]. Safe if absent. */
    @JvmStatic
    fun removeKey(alias: String) {
        try {
            KeyStore.getInstance(KEYSTORE).apply { load(null) }.deleteEntry(alias)
        } catch (_: Exception) {
        }
    }

    // ------------------------------------------------------------------------

    private fun generateKey(alias: String): SecretKey {
        val builder = KeyGenParameterSpec.Builder(
            alias,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .setUserAuthenticationRequired(true)
            .setUnlockedDeviceRequired(true)
            .setInvalidatedByBiometricEnrollment(true)
            // Auth-per-use, unlockable by fingerprint/face OR the device
            // credential. Note: with DEVICE_CREDENTIAL authorized, the OS does
            // NOT invalidate on new-biometric enrollment (the credential path
            // is unaffected) — invalidation instead fires on lockscreen
            // removal. Acceptable: someone who can enroll a finger already
            // knows the device credential.
            .setUserAuthenticationParameters(
                0,
                KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL
            )
        try {
            val kg = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
            kg.init(builder.setIsStrongBoxBacked(true).build())
            return kg.generateKey()
        } catch (_: Exception) {
            builder.setIsStrongBoxBacked(false)
        }
        val kg = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        kg.init(builder.build())
        return kg.generateKey()
    }

    private fun prompt(
        activity: Activity,
        requestId: Int,
        cipher: Cipher,
        title: String,
        op: (Cipher) -> Unit
    ) {
        val executor = activity.mainExecutor
        val callback = object : BiometricPrompt.AuthenticationCallback() {
            override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                try {
                    op(result.cryptoObject?.cipher ?: cipher)
                } catch (e: AEADBadTagException) {
                    // Wrapped blob and keystore key disagree (restored DB,
                    // alias collision) — dead enrollment, not a retryable error.
                    finish(requestId, JSONObject().put("invalidated", true).toString())
                } catch (e: Exception) {
                    finish(requestId, err(e))
                }
            }

            override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                // Lockouts fall back to the Vector PIN quietly — hammering a
                // throttled sensor with retries only extends the lockout.
                val cancelled = errorCode == BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED
                    || errorCode == BiometricPrompt.BIOMETRIC_ERROR_CANCELED
                    || errorCode == BiometricPrompt.BIOMETRIC_ERROR_LOCKOUT
                    || errorCode == BiometricPrompt.BIOMETRIC_ERROR_LOCKOUT_PERMANENT
                finish(
                    requestId,
                    if (cancelled) JSONObject().put("cancelled", true).toString()
                    else JSONObject().put("error", errString.toString()).toString()
                )
            }
            // onAuthenticationFailed = one bad read; the prompt stays up, no report.
        }

        val prompt = BiometricPrompt.Builder(activity)
            .setTitle(title)
            .setAllowedAuthenticators(
                BiometricManager.Authenticators.BIOMETRIC_STRONG
                    or BiometricManager.Authenticators.DEVICE_CREDENTIAL
            )
            .build()
        prompt.authenticate(BiometricPrompt.CryptoObject(cipher), CancellationSignal(), executor, callback)
    }

    private fun finish(requestId: Int, json: String) {
        try {
            nativeOnBiometricResult(requestId, json)
        } catch (_: Throwable) {
        }
    }

    private fun err(e: Exception): String {
        val msg = (e.message ?: e.javaClass.simpleName).replace("\"", "'")
        return JSONObject().put("error", msg).toString()
    }
}
