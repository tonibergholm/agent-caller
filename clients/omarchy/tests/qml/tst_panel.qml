// Headless wiring tests for Panel.qml: the real component, run against the
// stubs in tests/stubs. They prove what text matching cannot: that the
// whole-card-visible gate (cardFits) really stops a click and a key, that a
// card taller than the panel is read-only, and that selection scrolls.
import QtQuick
import QtTest
import "../.."
import "../../model.mjs" as Model

TestCase {
    id: tc
    name: "Panel"
    width: 400
    height: 300
    visible: true
    when: windowShown

    Component {
        id: serviceComp
        QtObject {
            property var view: null
            property var settings: ({ machines: [], show_idle: false, show_subagents: false, show_hook_only: false, notifications: true })
            property bool connected: true
            property int overflow: -1
            property var omitted: ({ done: 0, working: 0 })
            property var sent: []
            readonly property var tracker: ({ started: function() {}, settles: function() { return false; } })
            signal outcome(var msg)
            signal outcomesLost(var cards)
            signal ready()
            function send(type, fields, done) { sent.push({ type: type, fields: fields }); }
            function sentOf(type) { return sent.filter(s => s.type === type); }
        }
    }

    Component {
        id: hostComp
        QtObject {
            property bool opened: true
            property string pendingCard: ""
            function open() {}
            function close() {}
            function toggle() {}
            function switchPanel(d) {}
        }
    }

    Component {
        id: panelComp
        Panel {
            width: 400
            height: 300
        }
    }

    function pending(id, detail) {
        return {
            id: id, revision: 1, agent_id: "a1", kind: "permission", status: "open", answer_path: "hook",
            title: "Bash", detail: detail, free_text: false, raw_screen: null, questions: null,
            opened_at: "2026-10-08T00:00:00Z",
            options: [
                { id: "allow", label: "Yes", description: null, effect: "allow" },
                { id: "deny", label: "No", description: null, effect: "deny" }
            ]
        };
    }

    function card(key, detail) {
        return {
            key: key, machine_key: "m1", machine_name: "Box", group: "request", live: true, offline_since_ms: null,
            answerable: true, free_text_answerable: false, selectable_options: ["allow", "deny"],
            can_reply: true, can_mark_seen: false, busy: false, notice: null, action: null,
            agent: { id: key, source: "herdr", kind: "claude", title: "agent", project: "api", cwd: "/w/api", state: "needs_you", pending: pending("r-" + key, detail) }
        };
    }

    function view(cards) {
        return {
            machines: [{ key: "m1", name: "Box", machine_id: "x", status: { kind: "online" }, is_local: false, duplicate_of: null }],
            cards: cards, hidden_idle: 0, show_idle: false, show_subagents: false, show_hook_only: false,
            tray: { color: "amber", needs_you: cards.length }, actions: []
        };
    }

    // `panel.height` is small on purpose: the long card can never fit in it.
    function make(cards, panelHeight) {
        const svc = createTemporaryObject(serviceComp, tc);
        const host = createTemporaryObject(hostComp, tc);
        const panel = createTemporaryObject(panelComp, tc, { service: svc, host: host, height: panelHeight });
        verify(panel !== null);
        svc.view = view(cards);
        return { svc: svc, host: host, panel: panel };
    }

    function descendants(item, out) {
        out = out || [];
        const kids = [];
        for (let i = 0; i < item.children.length; i++) kids.push(item.children[i]);
        if (item.contentItem) kids.push(item.contentItem);
        for (const k of kids) {
            out.push(k);
            descendants(k, out);
        }
        return out;
    }

    function flickOf(panel) {
        return descendants(panel).filter(o => o.contentY !== undefined && o.contentHeight !== undefined && o.flickableDirection !== undefined && o.clip)[0];
    }

    function longDetail() {
        let s = "";
        for (let i = 0; i < 400; i++) s += "word" + i + " ";
        return s;
    }

    function optionAreas(item) {
        return descendants(item).filter(o => o.enabled !== undefined && o.containsMouse !== undefined && o.parent && o.parent.text !== undefined && /^\d\. /.test(o.parent.text));
    }

    // (a) A card taller than the panel is read-only and says so.
    function test_too_tall_card_is_read_only() {
        const t = make([card("m1/tall", longDetail())], 200);
        const item = t.panel.cardItemFor("m1/tall");
        verify(item !== null);
        tryVerify(() => item.height > t.panel.height, 5000, "the fixture card must be taller than the panel");
        verify(t.panel.cardTooTall(item));
        const notes = descendants(item).filter(o => o.text === Model.TOO_LONG_NOTE);
        compare(notes.length, 1);
        compare(Model.TOO_LONG_NOTE, "Too long to show here. Answer it in the terminal.");
        verify(notes[0].visible);
        const areas = optionAreas(item);
        compare(areas.length, 2);
        for (const a of areas) verify(!a.enabled, "option rows of a too-tall card are not clickable");
        // The gate itself, not only the disabled area: a direct call and a key are refused too.
        t.panel.pickOption(item.card, "allow");
        compare(t.panel.message, Model.HIDDEN_NOTICE);
        t.panel.catcher.textKey("1", 0);
        compare(t.svc.sentOf("pick").length, 0);
    }

    // (b) The gate follows the live geometry: clicks and keys on a card that
    // is not wholly in view send nothing; once it is, both send.
    function test_gate_disables_click_and_keys_when_card_not_fully_visible() {
        const t = make([card("m1/a", "npm test"), card("m1/b", "npm run build")], 300);
        const flick = flickOf(t.panel);
        verify(flick);
        const item = t.panel.cardItemFor("m1/a");
        verify(!t.panel.cardTooTall(item));
        flick.contentY = 0;
        verify(t.panel.cardFits("m1/a"));
        // Partly scrolled out of view: the top edge is above the viewport.
        item.y = 40;
        flick.contentY = 0;
        const savedY = item.y;
        flick.contentY = savedY + 10;
        verify(!t.panel.cardFits("m1/a"));
        const area = optionAreas(item)[0];
        verify(area);
        t.panel.keyState = Model.selectCard("m1/a");
        mouseClick(area);
        compare(t.panel.message, Model.HIDDEN_NOTICE);
        t.panel.catcher.textKey("1", 0);
        compare(t.svc.sentOf("pick").length, 0, "no pick while the card is partly hidden");
        // Wholly visible again: a click sends, and so does a key.
        flick.contentY = 0;
        item.y = 0;
        verify(t.panel.cardFits("m1/a"));
        mouseClick(area);
        compare(t.svc.sentOf("pick").length, 1);
        t.panel.keyState = Model.selectCard("m1/a");
        t.panel.catcher.textKey("2", 0);
        compare(t.svc.sentOf("pick").length, 2);
    }

    // (c) J and K move the selection and scroll the card into view.
    function test_j_and_k_select_and_scroll() {
        const cards = [card("m1/a", "one"), card("m1/b", "two"), card("m1/c", "three")];
        const t = make(cards, 260);
        const flick = flickOf(t.panel);
        tryVerify(() => flick.contentHeight > flick.height, 5000, "the list must scroll");
        t.panel.catcher.textKey("j", 0);
        compare(t.panel.keyState.selected, "m1/a");
        t.panel.catcher.textKey("j", 0);
        t.panel.catcher.textKey("j", 0);
        compare(t.panel.keyState.selected, "m1/c");
        const last = t.panel.cardItemFor("m1/c");
        verify(flick.contentY > 0, "selecting the last card scrolled the list");
        verify(last.y + last.height <= flick.contentY + flick.height + 0.5, "the selected card is wholly in view: y=" + last.y + " h=" + last.height + " cy=" + flick.contentY + " fh=" + flick.height + " ch=" + flick.contentHeight);
        t.panel.catcher.textKey("k", 0);
        t.panel.catcher.textKey("k", 0);
        compare(t.panel.keyState.selected, "m1/a");
        const first = t.panel.cardItemFor("m1/a");
        verify(first.y >= flick.contentY - 0.5, "scrolling back shows the first card again");
        verify(t.panel.cardFits("m1/a"));
    }

    // (d) No label inside a card limits its lines: the permission detail is shown whole.
    function test_no_label_in_a_card_inherits_maximum_line_count() {
        const t = make([card("m1/a", longDetail())], 600);
        const item = t.panel.cardItemFor("m1/a");
        const labels = descendants(item).filter(o => o.maximumLineCount !== undefined);
        verify(labels.length >= 4);
        for (const l of labels) compare(l.maximumLineCount, 2147483647, "label '" + String(l.text).slice(0, 20) + "'");
    }
}
