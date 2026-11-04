package dev.agentcaller.inbox;

import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;

import java.security.KeyStore;
import java.util.Arrays;

import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/**
 * An AES-256-GCM key that lives in the Android Keystore and never leaves it.
 * The native side asks this class to wrap and unwrap machine secrets, which it
 * keeps in files in the app's private directory. The key name is bound into
 * every blob as GCM associated data, so a blob moved to another file fails.
 *
 * Contract with jni_glue.rs, for both methods: they never throw and never log.
 * They return null when the Keystore cannot be used, an empty array when the
 * cipher refuses (no such key, wrong AAD, damaged blob), and bytes otherwise.
 * Blob layout: 12-byte IV, then ciphertext and 16-byte tag.
 */
final class KeystoreCipher {
    private static final String PROVIDER = "AndroidKeyStore";
    private static final String ALIAS = "dev.agentcaller.inbox.secrets.v1";
    private static final String TRANSFORM = "AES/GCM/NoPadding";
    private static final int IV_LEN = 12;
    private static final int TAG_BITS = 128;

    byte[] wrap(byte[] aad, byte[] plaintext) {
        try {
            SecretKey key = key(true);
            if (key == null) return null;
            Cipher cipher = Cipher.getInstance(TRANSFORM);
            cipher.init(Cipher.ENCRYPT_MODE, key);
            cipher.updateAAD(aad);
            byte[] iv = cipher.getIV();
            if (iv == null || iv.length != IV_LEN) return null;
            byte[] ct = cipher.doFinal(plaintext);
            byte[] blob = new byte[iv.length + ct.length];
            System.arraycopy(iv, 0, blob, 0, iv.length);
            System.arraycopy(ct, 0, blob, iv.length, ct.length);
            return blob;
        } catch (Throwable t) {
            return null;
        }
    }

    byte[] unwrap(byte[] aad, byte[] blob) {
        try {
            if (blob == null || blob.length <= IV_LEN) return new byte[0];
            SecretKey key = key(false);
            // The file exists but its key is gone (for example after a reset
            // of the lock screen): refuse, so the machine shows as unreadable
            // and is paired again. Never create a key here.
            if (key == null) return new byte[0];
            Cipher cipher = Cipher.getInstance(TRANSFORM);
            cipher.init(Cipher.DECRYPT_MODE, key,
                    new GCMParameterSpec(TAG_BITS, Arrays.copyOfRange(blob, 0, IV_LEN)));
            cipher.updateAAD(aad);
            byte[] plain = cipher.doFinal(blob, IV_LEN, blob.length - IV_LEN);
            return plain == null ? new byte[0] : plain;
        } catch (javax.crypto.AEADBadTagException e) {
            return new byte[0];
        } catch (javax.crypto.BadPaddingException e) {
            return new byte[0];
        } catch (Throwable t) {
            return null;
        }
    }

    /** The key, created on first use when {@code create}; null if absent and not created. */
    private static SecretKey key(boolean create) throws Exception {
        KeyStore store = KeyStore.getInstance(PROVIDER);
        store.load(null);
        if (store.containsAlias(ALIAS)) {
            return (SecretKey) store.getKey(ALIAS, null);
        }
        if (!create) return null;
        KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER);
        generator.init(new KeyGenParameterSpec.Builder(
                ALIAS, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build());
        return generator.generateKey();
    }
}
