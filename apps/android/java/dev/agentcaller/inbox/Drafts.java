package dev.agentcaller.inbox;

import java.util.HashMap;
import java.util.Iterator;
import java.util.Map;
import java.util.Set;

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

    /** Drops every draft whose request is not in {@code pending} (made by {@link #key}). */
    void prune(Set<String> pending) {
        Iterator<String> it = byRequest.keySet().iterator();
        while (it.hasNext()) {
            if (!pending.contains(it.next())) it.remove();
        }
    }
}
