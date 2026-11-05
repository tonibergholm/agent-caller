package dev.agentcaller.inbox;

import java.io.InputStream;
import java.io.OutputStream;
import java.security.Key;
import java.security.KeyStoreException;
import java.security.KeyStoreSpi;
import java.security.Provider;
import java.security.ProviderException;
import java.security.SecureRandom;
import java.security.UnrecoverableKeyException;
import java.security.cert.Certificate;
import java.security.spec.AlgorithmParameterSpec;
import java.util.Collections;
import java.util.Date;
import java.util.Enumeration;
import java.util.HashMap;
import java.util.Map;

import javax.crypto.KeyGeneratorSpi;
import javax.crypto.SecretKey;
import javax.crypto.spec.SecretKeySpec;

/**
 * A programmable "AndroidKeyStore" JCA provider for the host JVM, so the REAL
 * KeystoreCipher.java (not a double) runs against it. It models only what
 * KeystoreCipher asks of the platform: containsAlias, getKey, deleteEntry and
 * a KeyGenerator that stores the new key under the alias in the KeyGenParameterSpec.
 * Failures are injected through the static fields below.
 */
public final class FakeAndroidKeyStore extends Provider {
    static final Map<String, Key> entries = new HashMap<String, Key>();
    static volatile Exception getKeyFails;      // thrown by getKey (checked, via sneaky throw)
    static volatile RuntimeException containsFails;
    static volatile boolean generateFails;
    static volatile int deletes;
    static volatile int generations;

    public FakeAndroidKeyStore() {
        super("AndroidKeyStore", "1", "host-JVM fake of the Android Keystore");
        put("KeyStore.AndroidKeyStore", Spi.class.getName());
        put("KeyGenerator.AES", Gen.class.getName());
    }

    static void reset() {
        entries.clear();
        getKeyFails = null;
        containsFails = null;
        generateFails = false;
        deletes = 0;
        generations = 0;
    }

    /** A key whose every use fails the way an invalidated Keystore key does. */
    static final class DeadKey implements SecretKey {
        public String getAlgorithm() { return "AES"; }
        public String getFormat() { return "RAW"; }
        public byte[] getEncoded() {
            FakeAndroidKeyStore.<RuntimeException>sneaky(new android.security.keystore.KeyPermanentlyInvalidatedException());
            return null;
        }
    }

    @SuppressWarnings("unchecked")
    static <T extends Throwable> void sneaky(Throwable t) throws T {
        throw (T) t;
    }

    public static final class Gen extends KeyGeneratorSpi {
        private String alias;
        protected void engineInit(SecureRandom random) { throw new UnsupportedOperationException(); }
        protected void engineInit(int bits, SecureRandom random) { throw new UnsupportedOperationException(); }
        protected void engineInit(AlgorithmParameterSpec spec, SecureRandom random) {
            alias = ((android.security.keystore.KeyGenParameterSpec) spec).getKeystoreAlias();
        }
        protected SecretKey engineGenerateKey() {
            if (generateFails) throw new ProviderException("generation failed");
            byte[] k = new byte[32];
            new SecureRandom().nextBytes(k);
            SecretKey key = new SecretKeySpec(k, "AES");
            entries.put(alias, key);
            generations++;
            return key;
        }
    }

    public static final class Spi extends KeyStoreSpi {
        public Key engineGetKey(String alias, char[] password) throws UnrecoverableKeyException {
            Exception f = getKeyFails;
            if (f != null) FakeAndroidKeyStore.<RuntimeException>sneaky(f);
            return entries.get(alias);
        }
        public boolean engineContainsAlias(String alias) {
            RuntimeException f = containsFails;
            if (f != null) throw f;
            return entries.containsKey(alias);
        }
        public void engineDeleteEntry(String alias) { entries.remove(alias); deletes++; }
        public Certificate[] engineGetCertificateChain(String alias) { return null; }
        public Certificate engineGetCertificate(String alias) { return null; }
        public Date engineGetCreationDate(String alias) { return null; }
        public void engineSetKeyEntry(String a, Key k, char[] p, Certificate[] c) throws KeyStoreException { throw new KeyStoreException(); }
        public void engineSetKeyEntry(String a, byte[] k, Certificate[] c) throws KeyStoreException { throw new KeyStoreException(); }
        public void engineSetCertificateEntry(String a, Certificate c) throws KeyStoreException { throw new KeyStoreException(); }
        public Enumeration<String> engineAliases() { return Collections.enumeration(entries.keySet()); }
        public int engineSize() { return entries.size(); }
        public boolean engineIsKeyEntry(String alias) { return entries.containsKey(alias); }
        public boolean engineIsCertificateEntry(String alias) { return false; }
        public String engineGetCertificateAlias(Certificate c) { return null; }
        public void engineStore(OutputStream s, char[] p) {}
        public void engineLoad(InputStream s, char[] p) {}
    }
}
