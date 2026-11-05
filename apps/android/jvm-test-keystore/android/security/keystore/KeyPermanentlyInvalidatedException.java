package android.security.keystore;

import java.security.InvalidKeyException;

/** Host-JVM stand-in: same superclass as the platform's. */
public class KeyPermanentlyInvalidatedException extends InvalidKeyException {
    public KeyPermanentlyInvalidatedException() {
        super("Key permanently invalidated");
    }
}
