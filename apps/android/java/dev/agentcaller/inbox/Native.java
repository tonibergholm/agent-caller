package dev.agentcaller.inbox;

/**
 * The native library (Rust, crates/acd-app-core + apps/android). Every call
 * takes and returns strings; the replies are JSON. Nothing here logs.
 */
final class Native {
    private Native() {}

    static {
        System.loadLibrary("agent_caller_android");
    }

    /** Starts the inbox. Returns null when it runs, else the reason. */
    static native String init(String filesDir, KeystoreCipher cipher);

    /** The screen as JSON, or {"error": "..."}. */
    static native String view();

    /** {"ok": "<machine name>"} or {"error": "..."}. The error never echoes the text. */
    static native String importPairing(String text);

    static native String pick(String cardKey, String requestId, String optionId, long revision);

    static native String answerText(String cardKey, String requestId, long revision, String text);

    static native boolean cancel(String actionId);

    static native String markSeen(String cardKey);

    static native String removeMachine(String key);

    /**
     * The activity has put {@code text} on screen. Clears the pending-recovery
     * record when it is the current recovery notice. Blocks on file I/O: call
     * it off the main thread.
     */
    static native boolean noticeShown(String text);

    /** Blocks until the view lists n machines. For the host JVM test only. */
    static native boolean waitForMachines(long n);
}
