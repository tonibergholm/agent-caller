package android.security.keystore;

import java.security.spec.AlgorithmParameterSpec;

/** Host-JVM stand-in: carries the alias to the fake KeyGenerator, nothing else. */
public final class KeyGenParameterSpec implements AlgorithmParameterSpec {
    private final String alias;

    private KeyGenParameterSpec(String alias) {
        this.alias = alias;
    }

    public String getKeystoreAlias() {
        return alias;
    }

    public static final class Builder {
        private final String alias;

        public Builder(String alias, int purposes) {
            this.alias = alias;
        }

        public Builder setBlockModes(String... modes) {
            return this;
        }

        public Builder setEncryptionPaddings(String... paddings) {
            return this;
        }

        public Builder setKeySize(int bits) {
            return this;
        }

        public KeyGenParameterSpec build() {
            return new KeyGenParameterSpec(alias);
        }
    }
}
