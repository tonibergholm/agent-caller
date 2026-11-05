package dev.agentcaller.inbox;

import java.security.SecureRandom;
import java.util.Arrays;

import javax.crypto.Cipher;
import javax.crypto.spec.GCMParameterSpec;
import javax.crypto.spec.SecretKeySpec;

/**
 * Host-JVM double of java/dev/agentcaller/inbox/KeystoreCipher.java, for
 * jvm-test.sh only (never packaged). Same package, same method contract: null
 * when the "Keystore" is unavailable, an empty array when the cipher refuses,
 * bytes otherwise. The key is an in-memory AES-256 key persisted in a file
 * next to the test data so a second JVM run can read what the first wrote.
 */
final class KeystoreCipher {
    static volatile boolean unavailable = false;
    /** The key is permanently invalid (an invalidated Keystore key) until resetKey(). */
    static volatile boolean invalidated = false;
    static volatile int resets = 0;
    static volatile int calls = 0;
    static volatile String lastThread = "";
    private final SecretKeySpec key;

    KeystoreCipher(byte[] keyBytes) {
        key = new SecretKeySpec(keyBytes, "AES");
    }

    byte[] wrap(byte[] aad, byte[] plaintext) {
        calls++;
        lastThread = Thread.currentThread().getName();
        if (unavailable || invalidated) return null;
        try {
            byte[] iv = new byte[12];
            new SecureRandom().nextBytes(iv);
            Cipher c = Cipher.getInstance("AES/GCM/NoPadding");
            c.init(Cipher.ENCRYPT_MODE, key, new GCMParameterSpec(128, iv));
            c.updateAAD(aad);
            byte[] ct = c.doFinal(plaintext);
            byte[] blob = new byte[12 + ct.length];
            System.arraycopy(iv, 0, blob, 0, 12);
            System.arraycopy(ct, 0, blob, 12, ct.length);
            return blob;
        } catch (Throwable t) {
            return null;
        }
    }

    byte[] unwrap(byte[] aad, byte[] blob) {
        calls++;
        lastThread = Thread.currentThread().getName();
        if (unavailable || invalidated) return null;
        try {
            if (blob == null || blob.length <= 12) return new byte[0];
            Cipher c = Cipher.getInstance("AES/GCM/NoPadding");
            c.init(Cipher.DECRYPT_MODE, key, new GCMParameterSpec(128, Arrays.copyOfRange(blob, 0, 12)));
            c.updateAAD(aad);
            return c.doFinal(blob, 12, blob.length - 12);
        } catch (javax.crypto.AEADBadTagException e) {
            return new byte[0];
        } catch (Throwable t) {
            return null;
        }
    }

    int keyState() {
        return invalidated ? 1 : (unavailable ? 2 : 0);
    }

    int resetKey() {
        resets++;
        invalidated = false;
        return 0;
    }
}
