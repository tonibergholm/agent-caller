package dev.agentcaller.inbox;

import java.security.KeyStoreException;
import java.security.Security;
import java.security.UnrecoverableKeyException;

/**
 * Runs the REAL java/dev/agentcaller/inbox/KeystoreCipher.java against
 * FakeAndroidKeyStore: the permanent-versus-unknown classification, the alias
 * deletion in resetKey, and the null/empty contract of wrap and unwrap. What it
 * cannot show: that the real Android Keystore throws these exceptions in these
 * places (see docs/android.md).
 */
public class KeystoreCipherTest {
    static int failures = 0;

    static void check(boolean ok, String what) {
        System.out.println((ok ? "PASS " : "FAIL ") + what);
        if (!ok) failures++;
    }

    static final String ALIAS = "dev.agentcaller.inbox.secrets.v1";
    static final byte[] AAD = "aad".getBytes();
    static final byte[] AAD2 = "other".getBytes();

    public static void main(String[] args) throws Exception {
        Security.addProvider(new FakeAndroidKeyStore());
        KeystoreCipher c = new KeystoreCipher();

        FakeAndroidKeyStore.reset();
        check(c.keyState() == KeystoreCipher.KEY_USABLE, "no alias yet is usable (the first wrap creates it)");
        byte[] blob = c.wrap(AAD, "secret".getBytes());
        check(blob != null && blob.length > 12, "wrap creates the key and returns bytes");
        check(FakeAndroidKeyStore.generations == 1, "exactly one key was generated");
        check(c.keyState() == KeystoreCipher.KEY_USABLE, "a healthy key is usable");
        check(new String(c.unwrap(AAD, blob)).equals("secret"), "unwrap round-trips");
        byte[] wrongAad = c.unwrap(AAD2, blob);
        check(wrongAad != null && wrongAad.length == 0, "wrong AAD: empty array (the cipher refused), not null");
        byte[] damaged = blob.clone();
        damaged[damaged.length - 1] ^= 1;
        byte[] bad = c.unwrap(AAD, damaged);
        check(bad != null && bad.length == 0, "damaged blob: empty array");

        // Permanent: UnrecoverableKeyException from getKey, direct or in the cause chain.
        FakeAndroidKeyStore.getKeyFails = new UnrecoverableKeyException("gone");
        check(c.keyState() == KeystoreCipher.KEY_PERMANENTLY_UNUSABLE, "UnrecoverableKeyException is permanent");
        FakeAndroidKeyStore.getKeyFails = new KeyStoreException("wrapped", new UnrecoverableKeyException("gone"));
        check(c.keyState() == KeystoreCipher.KEY_PERMANENTLY_UNUSABLE, "permanent is found in the cause chain");
        // ...but never at the wrap/unwrap level: those say "cannot use now" (null), never "refused" (empty).
        check(c.unwrap(AAD, blob) == null, "unwrap with a dead key reports unavailable (null), so nothing is read as refused");
        check(c.wrap(AAD, "x".getBytes()) == null, "wrap with a dead key reports unavailable (null)");

        // Permanent: a key whose use throws KeyPermanentlyInvalidatedException.
        FakeAndroidKeyStore.getKeyFails = null;
        FakeAndroidKeyStore.entries.put(ALIAS, new FakeAndroidKeyStore.DeadKey());
        check(c.keyState() == KeystoreCipher.KEY_PERMANENTLY_UNUSABLE, "KeyPermanentlyInvalidatedException on use is permanent");

        // Unknown: anything else. Unknown must never be reported permanent (it would delete pairings).
        FakeAndroidKeyStore.reset();
        FakeAndroidKeyStore.entries.put(ALIAS, new javax.crypto.spec.SecretKeySpec(new byte[32], "AES"));
        FakeAndroidKeyStore.getKeyFails = new KeyStoreException("provider busy");
        check(c.keyState() == KeystoreCipher.KEY_UNKNOWN, "a KeyStoreException is unknown, not permanent");
        FakeAndroidKeyStore.getKeyFails = new java.security.ProviderException("transient");
        check(c.keyState() == KeystoreCipher.KEY_UNKNOWN, "a ProviderException is unknown");
        FakeAndroidKeyStore.getKeyFails = null;
        FakeAndroidKeyStore.containsFails = new IllegalStateException("keystore not loaded");
        check(c.keyState() == KeystoreCipher.KEY_UNKNOWN, "a failure before the key is read is unknown");
        FakeAndroidKeyStore.containsFails = null;

        // resetKey: the real alias deletion, then a fresh key.
        FakeAndroidKeyStore.reset();
        FakeAndroidKeyStore.entries.put(ALIAS, new FakeAndroidKeyStore.DeadKey());
        check(c.resetKey() == 0, "resetKey reports a fresh key");
        check(FakeAndroidKeyStore.deletes == 1, "resetKey deleted the alias");
        check(FakeAndroidKeyStore.generations == 1, "resetKey generated a new key");
        check(!(FakeAndroidKeyStore.entries.get(ALIAS) instanceof FakeAndroidKeyStore.DeadKey), "the dead key is gone");
        check(c.keyState() == KeystoreCipher.KEY_USABLE, "the new key is usable");
        byte[] fresh = c.wrap(AAD, "again".getBytes());
        check(fresh != null && new String(c.unwrap(AAD, fresh)).equals("again"), "the new key wraps and unwraps");

        // resetKey where generation fails after the alias was deleted.
        FakeAndroidKeyStore.reset();
        FakeAndroidKeyStore.entries.put(ALIAS, new FakeAndroidKeyStore.DeadKey());
        FakeAndroidKeyStore.generateFails = true;
        check(c.resetKey() == 1, "resetKey reports failure when no new key was made");
        check(FakeAndroidKeyStore.deletes == 1 && !FakeAndroidKeyStore.entries.containsKey(ALIAS), "the alias is gone");
        check(c.keyState() == KeystoreCipher.KEY_USABLE, "no alias reads as usable: the pending-recovery record is what tells the user");
        check(c.wrap(AAD, "x".getBytes()) == null, "wrap cannot create a key while generation fails");
        FakeAndroidKeyStore.generateFails = false;
        check(c.wrap(AAD, "x".getBytes()) != null, "the next wrap creates the key once generation works");

        // unwrap never creates a key.
        FakeAndroidKeyStore.reset();
        byte[] none = c.unwrap(AAD, new byte[40]);
        check(none != null && none.length == 0 && FakeAndroidKeyStore.generations == 0, "unwrap with no key refuses and creates nothing");

        System.out.println(failures == 0 ? "ALL PASS" : failures + " FAILED");
        System.exit(failures == 0 ? 0 : 1);
    }
}
