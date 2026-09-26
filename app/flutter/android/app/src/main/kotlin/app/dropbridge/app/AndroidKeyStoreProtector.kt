package app.dropbridge.app

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import java.io.File
import java.security.KeyStore
import java.security.SecureRandom
import java.util.Arrays
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * 2026 Android Hardware-backed Keystore / TEE / StrongBox Identity Protector.
 *
 * Protects the permanent 32-byte Ed25519 identity seed at rest using hardware-backed
 * AES-256-GCM keys. The raw seed never exists in plaintext on flash storage.
 */
class AndroidKeyStoreProtector(private val context: Context) {

    private val keyStore: KeyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply {
        load(null)
    }

    /**
     * Retrieves or generates the 32-byte Ed25519 identity seed.
     * The returned array should be zeroized after being passed into native memory.
     */
    fun getOrCreateIdentitySeed(): ByteArray {
        val seedFile = File(context.filesDir, SEALED_SEED_FILENAME)
        if (seedFile.exists()) {
            val sealedBytes = seedFile.readBytes()
            return unseal(sealedBytes)
        }

        // Generate a cryptographically secure 32-byte seed
        val freshSeed = ByteArray(SEED_LENGTH_BYTES).apply {
            SecureRandom().nextBytes(this)
        }
        val sealed = seal(freshSeed)
        seedFile.writeBytes(sealed)
        return freshSeed
    }

    private fun getOrCreateMasterKey(): SecretKey {
        if (keyStore.containsAlias(KEY_ALIAS)) {
            return keyStore.getKey(KEY_ALIAS, null) as SecretKey
        }

        // Try generating with hardware StrongBox (dedicated tamper-resistant hardware enclave),
        // and fall back to standard TEE (TrustZone / KeyMint) if StrongBox is unavailable.
        return try {
            generateAesKey(useStrongBox = true)
        } catch (e: Exception) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P && e is StrongBoxUnavailableException) {
                generateAesKey(useStrongBox = false)
            } else {
                generateAesKey(useStrongBox = false)
            }
        }
    }

    private fun generateAesKey(useStrongBox: Boolean): SecretKey {
        val keyGen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        val specBuilder = KeyGenParameterSpec.Builder(
            KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .setRandomizedEncryptionRequired(true)

        if (useStrongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            specBuilder.setIsStrongBoxBacked(true)
        }

        keyGen.init(specBuilder.build())
        return keyGen.generateKey()
    }

    /**
     * Seal (encrypt) raw seed bytes with AES-256-GCM.
     * Output format: [12 bytes IV] + [ciphertext + 16 bytes GCM tag]
     */
    fun seal(plaintext: ByteArray): ByteArray {
        val key = getOrCreateMasterKey()
        val cipher = Cipher.getInstance(AES_GCM_NO_PADDING)
        cipher.init(Cipher.ENCRYPT_MODE, key)
        val iv = cipher.iv
        val ciphertext = cipher.doFinal(plaintext)

        val out = ByteArray(iv.size + ciphertext.size)
        System.arraycopy(iv, 0, out, 0, iv.size)
        System.arraycopy(ciphertext, 0, out, iv.size, ciphertext.size)
        return out
    }

    /**
     * Unseal (decrypt) ciphertext with AES-256-GCM using hardware master key.
     */
    fun unseal(sealed: ByteArray): ByteArray {
        require(sealed.size > GCM_IV_LENGTH_BYTES) { "Sealed payload too short" }
        val key = getOrCreateMasterKey()
        val iv = sealed.copyOfRange(0, GCM_IV_LENGTH_BYTES)
        val ciphertext = sealed.copyOfRange(GCM_IV_LENGTH_BYTES, sealed.size)

        val cipher = Cipher.getInstance(AES_GCM_NO_PADDING)
        val gcmSpec = GCMParameterSpec(GCM_TAG_LENGTH_BITS, iv)
        cipher.init(Cipher.DECRYPT_MODE, key, gcmSpec)
        return cipher.doFinal(ciphertext)
    }

    companion object {
        private const val ANDROID_KEYSTORE = "AndroidKeyStore"
        private const val KEY_ALIAS = "dropbridge_device_master_key"
        private const val AES_GCM_NO_PADDING = "AES/GCM/NoPadding"
        private const val SEALED_SEED_FILENAME = "device_identity_sealed.bin"
        private const val SEED_LENGTH_BYTES = 32
        private const val GCM_IV_LENGTH_BYTES = 12
        private const val GCM_TAG_LENGTH_BITS = 128
    }
}
