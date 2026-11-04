#!/bin/sh
# Runs the real Rust library through the real JNI exports on the host JVM, with
# a software AES-GCM double in place of the Android Keystore. It exercises the
# JNI glue, JniKeyWrap, attach_current_thread from native worker threads, the
# wrapped-file store and the bridge together. It is NOT an Android run: bionic,
# the Keystore and the Dalvik/ART runtime are not involved.
#
# Run 6 compiles the REAL KeystoreCipher.java against a fake AndroidKeyStore
# provider and android.* stubs (jvm-test-keystore/); it needs no org.json.
#
# Needs: a JDK, org.json on the classpath (set ORG_JSON_JAR; the test fetches
# nothing), cargo.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"
: "${ORG_JSON_JAR:?set ORG_JSON_JAR to an org.json jar (apt: libandroid-json-org-java, /usr/share/java/json-latest.jar)}"
work=${1:-$here/build/jvm-test}
rm -rf "$work"; mkdir -p "$work/classes"
cargo build --lib --locked
lib=$here/target/debug
javac -Xlint:-options -cp "$ORG_JSON_JAR" -d "$work/classes" \
  java/dev/agentcaller/inbox/Native.java java/dev/agentcaller/inbox/NoticeTracker.java java/dev/agentcaller/inbox/Drafts.java jvm-test/dev/agentcaller/inbox/*.java jvm-test/HostJniTest.java
run() { java -Djava.library.path="$lib" -cp "$work/classes:$ORG_JSON_JAR" HostJniTest "$work/data" "$@"; }
echo "== run 1: pair"; run pair
echo "== run 2: restart and read the secret back"; run verify
echo "== run 3: restart with the Keystore unavailable"; run unavailable
echo "== run 4: damage the secret file, restart"
f=$(find "$work/data/files/secrets" -type f | head -1)
printf 'x' >> "$f"
run tampered
echo "== run 5: the Keystore key is permanently invalidated, restart"
run invalidated
echo "== run 6: the real KeystoreCipher.java against a fake AndroidKeyStore provider"
real=$work/classes-real
mkdir -p "$real"
javac -Xlint:-options -d "$real" java/dev/agentcaller/inbox/KeystoreCipher.java \
  jvm-test-keystore/android/security/keystore/*.java jvm-test-keystore/dev/agentcaller/inbox/*.java
java -cp "$real" dev.agentcaller.inbox.KeystoreCipherTest
echo "== run 7: NoticeTracker and Drafts (the presentation rules MainActivity uses)"
java -cp "$work/classes:$ORG_JSON_JAR" dev.agentcaller.inbox.PresentationTest
