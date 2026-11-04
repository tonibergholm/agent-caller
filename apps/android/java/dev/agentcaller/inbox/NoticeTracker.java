package dev.agentcaller.inbox;

/**
 * Decides when a recovery notice counts as acknowledged. No Android types, so
 * the host JVM test drives the same class the activity uses.
 *
 * Drawing a notice is not showing it: a refresh that completes after
 * {@code onPause()} still draws into a view nobody sees, and the process can be
 * killed before the user comes back. The pending-recovery record is deleted
 * only on an explicit tap on Dismiss, and a tap counts only while the activity
 * is resumed and a notice is on the screen. UI thread only.
 */
final class NoticeTracker {
    private boolean resumed = false;
    private String current = null; // the notice text in the latest view, if any
    private String dismissed = null; // the text the user dismissed

    void onResume() {
        resumed = true;
    }

    void onPause() {
        resumed = false;
    }

    /**
     * A view arrived (resumed or not). Returns the notice to put on screen, or
     * null for none. This never acknowledges anything.
     */
    String update(String note) {
        current = note;
        if (note == null) {
            dismissed = null; // gone from the view: an identical later notice is new
            return null;
        }
        if (note.equals(dismissed)) return null;
        return note;
    }

    /**
     * The user tapped Dismiss. Returns the text to acknowledge natively, or
     * null when the tap does not count (not resumed, or no notice on screen).
     */
    String dismiss() {
        if (!resumed || current == null || current.equals(dismissed)) return null;
        dismissed = current;
        return current;
    }
}
