package dev.agentcaller.inbox;

import java.io.File;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;

import org.json.JSONArray;
import org.json.JSONObject;

/** Drives the real Rust library through the real JNI exports on a host JVM. */
public class HostJniAccess {
    static int failures = 0;

    static void check(boolean ok, String what) {
        System.out.println((ok ? "PASS " : "FAIL ") + what);
        if (!ok) failures++;
    }

    /** The pin acd uses: "sha256:" and the unpadded URL-safe base64 of SHA-256 of the SPKI bytes. */
    static String pin() throws Exception {
        byte[] d = java.security.MessageDigest.getInstance("SHA-256").digest("test certificate".getBytes("UTF-8"));
        return "sha256:" + java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(d);
    }

    static String line(String host) throws Exception {
        // 43 'A's: a well-formed client token.
        return "acd://" + host + ":47800?pin=" + pin() + "#"
                + "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    }

    static boolean anyFileContains(Path dir, String needle) throws Exception {
        try (java.util.stream.Stream<Path> s = Files.walk(dir)) {
            for (Path p : (Iterable<Path>) s::iterator) {
                if (Files.isRegularFile(p) && new String(Files.readAllBytes(p), "ISO-8859-1").contains(needle)) {
                    return true;
                }
            }
        }
        return false;
    }

    static JSONObject view() throws Exception {
        return new JSONObject(Native.view());
    }

    static JSONObject viewWithMachines(int n) throws Exception {
        long end = System.currentTimeMillis() + 10000;
        while (true) {
            JSONObject v = view();
            if (v.has("machines") && v.getJSONArray("machines").length() == n) return v;
            if (System.currentTimeMillis() > end) return v;
            Thread.sleep(20);
        }
    }

    public static int run(String[] args) throws Exception {
        Path dir = Paths.get(args[0]);
        String mode = args[1];
        Files.createDirectories(dir);
        // The "Keystore" key lives next to the data so a second JVM can reuse it.
        Path keyFile = dir.resolve("test-key.bin");
        byte[] keyBytes;
        if (Files.exists(keyFile)) {
            keyBytes = Files.readAllBytes(keyFile);
        } else {
            keyBytes = new byte[32];
            new java.security.SecureRandom().nextBytes(keyBytes);
            Files.write(keyFile, keyBytes);
        }
        Path files = dir.resolve("files");
        Files.createDirectories(files);
        String token = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

        if (mode.equals("unavailable")) KeystoreCipher.unavailable = true;
        check(Native.view().contains("not started"), "view before init says the inbox is not started");
        String err = Native.init(files.toString(), new KeystoreCipher(keyBytes));
        check(err == null, "init returns null (" + err + ")");
        check(Native.init(files.toString(), new KeystoreCipher(keyBytes)) == null, "a second init is a no-op");

        if (mode.equals("pair")) {
            JSONObject v = view();
            check(v.getJSONArray("machines").length() == 0, "no machine before pairing");
            String bad = Native.importPairing("hello #SECRETSECRET");
            check(bad.contains("\"error\"") && !bad.contains("SECRETSECRET"), "a bad line is refused, text not echoed: " + bad);
            String lan = Native.importPairing("acd://192.168.1.10:47800?pin=" + pin() + "#" + token);
            check(lan.contains("Tailscale") && !lan.contains(token), "a LAN address is refused: " + lan);

            String ok = Native.importPairing("  " + line("example.test") + "\n");
            check(ok.startsWith("{\"ok\":"), "pairing succeeds: " + ok);
            check(KeystoreCipher.calls > 0, "the Keystore double was called through JNI (" + KeystoreCipher.calls + " calls)");
            check(viewWithMachines(1).getJSONArray("machines").length() == 1, "the machine is in the view");
            check(!anyFileContains(files, token), "the token is in no file under files/");
            check(Files.isDirectory(files.resolve("secrets")), "a secrets/ directory was created");

            // The Keystore goes away: a second pairing must fail closed and commit nothing.
            KeystoreCipher.unavailable = true;
            String blocked = Native.importPairing(line("other.test"));
            check(blocked.contains("\"error\"") && !blocked.contains(token), "pairing with the Keystore unavailable fails: " + blocked);
            KeystoreCipher.unavailable = false;
            check(view().getJSONArray("machines").length() == 1, "no second machine was committed");

            // Calls from other threads work (the JNI attach path).
            final String[] box = new String[1];
            Thread t = new Thread(new Runnable() {
                public void run() { box[0] = Native.view(); }
            }, "other-thread");
            t.start(); t.join();
            check(box[0] != null && box[0].contains("machines"), "view from another Java thread");

            check(!Native.cancel("nope"), "cancel of an unknown action is false");
            check(Native.pick("x:y", "r", "allow", 1L).contains("\"error\""), "pick on an unknown card is refused");
            check(Native.answerText("x:y", "r", 1L, "t").contains("\"error\""), "answer on an unknown card is refused");
            check(Native.removeMachine("missing").contains("\"error\""), "removing an unknown machine is refused");
        } else if (mode.equals("verify")) {
            // A new process: the workers read the secret back through the cipher, on native threads.
            JSONObject v = viewWithMachines(1);
            check(v.getJSONArray("machines").length() == 1, "the paired machine survived a restart");
            String s = v.toString();
            check(!s.contains("Pair this machine"), "the machine's secret was read back (not 'Pair this machine'): " + s);
            check(KeystoreCipher.calls > 0, "unwrap ran through JNI after restart (" + KeystoreCipher.calls + " calls, last thread " + KeystoreCipher.lastThread + ")");
        } else if (mode.equals("unavailable")) {
            // Start with the Keystore unusable (set before init above).
            JSONObject v = viewWithMachines(1);
            String s = v.toString();
            check(v.getJSONArray("machines").length() == 1, "the machine is listed");
            check(!s.contains("\"online\"") && s.contains("cannot be read here") && !s.contains("Pair this machine"),
                    "an unopenable secret says so and never reads as online or as not paired: " + s);
            check(!Native.pick("x:y", "r", "allow", 1L).contains("\"ok\""), "nothing can be sent");
        } else if (mode.equals("tampered")) {
            // The blob was damaged on disk: the machine must show as not paired/unreadable, never as paired.
            JSONObject v = viewWithMachines(1);
            String s = v.toString();
            check(s.contains("Pair this machine") || s.contains("unreadable") || s.contains("attention"),
                    "a damaged secret file does not read as a working pairing: " + s);
        }
        System.out.println(failures == 0 ? "ALL PASS" : failures + " FAILED");
        return failures == 0 ? 0 : 1;
    }
}
