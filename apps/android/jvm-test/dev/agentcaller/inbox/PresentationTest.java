package dev.agentcaller.inbox;

import java.util.HashSet;
import java.util.Set;

import org.json.JSONArray;

/** NoticeTracker and Drafts, the classes MainActivity uses. No Android, no native library. */
public class PresentationTest {
    static int failures = 0;

    static void check(boolean ok, String what) {
        if (!ok) {
            failures++;
            System.out.println("FAIL: " + what);
        }
    }

    /** One view's cards array. {@code pendingId}/{@code answerableId} null means absent. */
    static JSONArray view(String pendingId, String answerableId) throws Exception {
        return new JSONArray("[{\"key\":\"m:a\",\"request_id\":" + q(answerableId)
                + ",\"pending_request_id\":" + q(pendingId)
                + ",\"free_text\":" + (answerableId != null) + "}]");
    }

    static String q(String s) {
        return s == null ? "null" : "\"" + s + "\"";
    }

    public static void main(String[] args) throws Exception {
        // ---- notice: drawing is not showing ----
        NoticeTracker t = new NoticeTracker();
        t.onResume();
        check(t.dismiss() == null, "nothing on screen, nothing to dismiss");
        check("lost".equals(t.update("lost")), "a notice is drawn");
        t.onPause();
        check(t.dismiss() == null, "a tap while paused does not count");
        check("lost".equals(t.update("lost")), "a view arriving while paused still draws the notice");
        check(t.dismiss() == null, "still paused");
        t.onResume();
        check("lost".equals(t.dismiss()), "resumed: the tap counts once");
        check(t.dismiss() == null, "a second tap does not acknowledge twice");
        check(t.update("lost") == null, "a stale view with the dismissed text does not bring it back");
        check(t.update(null) == null, "the notice left the view");
        check("lost".equals(t.update("lost")), "an identical later notice is new and shown again");
        check("lost".equals(t.dismiss()), "and can be dismissed");

        // ---- drafts: per request, pruned to what is pending ----
        Drafts d = new Drafts();
        d.put("m:a", "r1", "typed for r1");
        d.put("m:b", "r9", "other agent");
        check("typed for r1".equals(d.get("m:a", "r1")), "the draft is found by card and request");
        check(d.get("m:a", "r2") == null, "a later question from the same agent does not inherit it");
        check(!Drafts.key("m", "xr").equals(Drafts.key("mx", "r")), "key parts do not run together");
        Set<String> pending = new HashSet<String>();
        pending.add(Drafts.key("m:a", "r2")); // r1 was answered elsewhere; m:b is gone
        d.prune(pending);
        check(d.size() == 0, "drafts of requests that are no longer pending are dropped (" + d.size() + " left)");
        d.put("m:a", "r2", "mine");
        pending.clear();
        pending.add(Drafts.key("m:a", "r2"));
        d.prune(pending);
        check("mine".equals(d.get("m:a", "r2")), "a pending request keeps its draft");
        d.remove("m:a", "r2");
        check(d.size() == 0, "remove drops it");

        // ---- drafts survive going offline: pending is not the same as answerable ----
        Drafts o = new Drafts();
        o.put("m:a", "r1", "typed");
        o.prune(Drafts.pendingKeys(view("r1", "r1")));
        check("typed".equals(o.get("m:a", "r1")), "online with r1: the draft stays");
        o.prune(Drafts.pendingKeys(view("r1", null))); // offline: pending, not answerable
        check("typed".equals(o.get("m:a", "r1")), "offline, r1 still pending: the draft stays");
        o.prune(Drafts.pendingKeys(view("r1", "r1")));
        check("typed".equals(o.get("m:a", "r1")), "back online, same r1: the draft is still there");
        o.prune(Drafts.pendingKeys(view("r2", "r2")));
        check(o.get("m:a", "r1") == null && o.size() == 0, "the card now holds r2: the r1 draft is dropped");
        o.put("m:a", "r2", "second");
        o.prune(Drafts.pendingKeys(new JSONArray("[]")));
        check(o.size() == 0, "the card is gone: the draft is dropped");
        o.put("m:a", "r3", "x");
        o.prune(Drafts.pendingKeys(view(null, null))); // the request resolved, card stays
        check(o.size() == 0, "the card has no pending request: the draft is dropped");

        System.out.println(failures == 0 ? "ALL PASS" : failures + " FAILED");
        System.exit(failures == 0 ? 0 : 1);
    }
}
