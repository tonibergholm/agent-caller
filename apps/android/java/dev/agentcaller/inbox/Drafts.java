package dev.agentcaller.inbox;

import java.util.HashMap;
import java.util.HashSet;
import java.util.Iterator;
import java.util.Map;
import java.util.Set;

import org.json.JSONArray;
import org.json.JSONObject;

/**
 * Typed-but-unsent answers, one per pending request. Keyed by card key AND
 * request id: a later question from the same agent is a different request and
 * never inherits the text typed for an earlier one. No Android types. UI
 * thread only.
 */
final class Drafts {
    private final Map<String, String> byRequest = new HashMap<String, String>();

    static String key(String cardKey, String requestId) {
        // The request id is the end of the key: a card key may hold any
        // character except through this length prefix.
        return cardKey.length() + ":" + cardKey + requestId;
    }

    String get(String cardKey, String requestId) {
        return byRequest.get(key(cardKey, requestId));
    }

    void put(String cardKey, String requestId, String text) {
        byRequest.put(key(cardKey, requestId), text);
    }

    void remove(String cardKey, String requestId) {
        byRequest.remove(key(cardKey, requestId));
    }

    int size() {
        return byRequest.size();
    }

    /** The (card, request) keys {@link #prune} keeps, from the view's {@code cards} array. */
    static Set<String> pendingKeys(JSONArray cards) throws org.json.JSONException {
        Set<String> pending = new HashSet<String>();
        for (int i = 0; i < cards.length(); i++) {
            JSONObject c = cards.getJSONObject(i);
            // Pending, not answerable: an offline card keeps the text typed for it.
            if (!c.isNull("pending_request_id")) {
                pending.add(key(c.getString("key"), c.getString("pending_request_id")));
            }
        }
        return pending;
    }

    /** Drops every draft whose request is not in {@code pending} (made by {@link #key}). */
    void prune(Set<String> pending) {
        Iterator<String> it = byRequest.keySet().iterator();
        while (it.hasNext()) {
            if (!pending.contains(it.next())) it.remove();
        }
    }
}
