// The inbox panel content (omarchy spec section 7), placed inside BarWidget's
// Ui.KeyboardPanel. What shows follows InboxView exactly; the machine filter
// is the panel's own. Logic and texts come from model.mjs; this file wires
// them to the shell's own widgets and palette (research note, sections 2, 8).
import QtQuick
import QtQuick.Layouts
import qs.Commons as Commons
import qs.Ui as Ui
import "model.mjs" as Model

Item {
    id: panel

    // The service client (Service.qml) and the Ui.Panel that owns the popup.
    required property var service
    required property var host

    readonly property bool shown: host.opened
    readonly property alias catcher: keyCatcher

    property var keyState: ({ selected: null, replyFor: null })
    // The request a typed answer was opened for (plan D34).
    property var answerEditor: null
    // Typed text outside the delegates, which every new view recreates (plan D35).
    property var drafts: new Model.Drafts()
    // The text field that has focus, or null; it blocks the key catcher.
    property var focusedField: null
    property string machineFilter: ""
    property bool settingsOpen: false
    property bool doneOpen: false
    property string message: ""
    property var uncertain: ({})
    property real now: Date.now()

    readonly property var view: service.view
    readonly property var cards: view ? view.cards : []
    readonly property var groups: Model.sections(cards, machineFilter === "" ? null : machineFilter)
    readonly property var visibleCards: groups.needs.concat(groups.working, doneOpen ? groups.done : [])
    readonly property var chips: Model.machineChips(view, service.settings)

    readonly property color fg: Commons.Color.foreground
    readonly property color muted: Commons.Color.muted

    function open() { host.open(); }
    function close() { host.close(); }
    function toggle() { host.toggle(); }

    function openCard(key) {
        settingsOpen = false;
        keyState = Model.selectCard(key);
        host.open();
    }

    function cardFor(key) {
        return cards.find(c => c.key === key) ?? null;
    }

    function report(reply) {
        message = reply.type === "error" ? Model.errorText(reply.code) : "";
    }

    function started(cardKey) {
        return reply => {
            report(reply);
            if (reply.type === "ok") service.tracker.started(reply.result, cardKey);
        };
    }

    // `input`: the focused text field, or null.
    function run(effect, input) {
        switch (effect.kind) {
        case "pick":
            service.send("pick", { card: effect.cardKey, request: effect.requestId, option: effect.optionId, revision: effect.revision }, started(effect.cardKey));
            break;
        case "focusAnswer":
            answerEditor = Model.answerEditor(cardFor(effect.cardKey));
            break;
        case "submitReply":
            if (submitReply(effect.cardKey, input.text)) input.text = "";
            break;
        case "submitAnswer":
            submitAnswer(input.text);
            break;
        case "submitPairing":
            submitPairing();
            break;
        case "openReply":
            keyState = Object.assign({}, keyState, { replyFor: effect.cardKey });
            break;
        case "markSeen":
            service.send("mark_seen", { card: effect.cardKey }, report);
            break;
        case "escape":
            // This panel's newest countdown, else close (spec section 6).
            service.send("escape", {}, reply => {
                if (Model.escapeCloses(reply)) close();
            });
            break;
        }
    }

    function submitReply(cardKey, text) {
        const r = Model.validateReply(text);
        if (!r.ok) {
            message = Model.reasonText(r.reason);
            return false;
        }
        service.send("reply", { card: cardKey, text: r.text }, started(cardKey));
        const card = cardFor(cardKey);
        if (card) drafts.clear(Model.draftKey.reply(card));
        keyState = Object.assign({}, keyState, { replyFor: null });
        return true;
    }

    // Sends to the request the editor was opened for, never the card's
    // current one; the plan is made again now, from the view on screen.
    // The text stays until the request resolves, so an undo gets it back.
    function submitAnswer(text) {
        const plan = Model.planAnswer(answerEditor, cards, text);
        switch (plan.kind) {
        case "closed":
            answerEditor = null;
            message = Model.EDITOR_CLOSED_TEXT;
            break;
        case "invalid":
            message = Model.reasonText(plan.reason);
            break;
        case "send":
            service.send("answer_text", plan.fields, started(plan.fields.card));
            break;
        }
    }

    // Every key goes through model.mjs: `field` says which text field has
    // focus, and `composing` is that field's IME state (spec section 7).
    function keyEvent(event, field, composing, input) {
        const r = Model.onKey(visibleCards, keyState, Model.fromQt(event.key, event.text, event.modifiers, composing, field));
        keyState = r.state;
        if (r.effect.kind !== "none") {
            run(r.effect, input);
            event.accepted = true;
        }
    }

    // The catcher's semantic signals, as the keys model.mjs knows.
    function catcherKey(key, text, modifiers) {
        keyEvent({ key: key, text: text, modifiers: modifiers, accepted: false }, null, false, null);
    }

    // A changed request closes its typed-answer editor; the text is dropped.
    onViewChanged: {
        if (answerEditor !== null && !Model.answerEditorValid(answerEditor, cards)) {
            answerEditor = null;
            message = Model.EDITOR_CLOSED_TEXT;
        }
        // No view (disconnected, overflow) is not a reason to drop state.
        if (view !== null) {
            keyState = Model.pruneKeyState(keyState, cards);
            drafts.prune(view, service.settings);
        }
    }

    // The service tells notification code whether a panel is open (spec section 8).
    onShownChanged: service.send("panel", { open: shown })

    // `openCard` sets pendingCard then opens; clearing it lets the same key be sent again.
    Connections {
        target: panel.host

        function onPendingCardChanged() {
            if (panel.host.pendingCard === "") return;
            panel.settingsOpen = false;
            panel.keyState = Model.selectCard(panel.host.pendingCard);
            panel.host.pendingCard = "";
        }
    }

    Connections {
        target: panel.service

        function onOutcomesLost(cards) {
            const u = Object.assign({}, panel.uncertain);
            for (const c of cards) u[c] = true;
            panel.uncertain = u;
        }

        // The service forgets a panel's state when the connection drops.
        function onReady() {
            panel.service.send("panel", { open: panel.shown });
        }
    }

    Timer {
        interval: 100
        repeat: true
        running: panel.shown && panel.view !== null && panel.view.actions.length > 0
        onTriggered: panel.now = Date.now()
    }

    // The pairing line is a secret: cleared before it is sent, after every
    // attempt; never logged; errors never repeat it (spec section 7).
    function submitPairing() {
        const line = pairingField.text;
        pairingField.text = "";
        if (line.trim() === "") return;
        service.send("pair", { pairing: line }, report);
    }

    component Label: Text {
        color: Commons.Color.foreground
        font.family: Commons.Style.font.family
        font.pixelSize: Commons.Style.font.body
        textFormat: Text.PlainText
    }

    component CardItem: Rectangle {
        id: cardItem

        required property var inbox
        required property var card
        readonly property var meta: Model.cardMeta(card)
        readonly property var rows: Model.optionRows(card)
        readonly property bool selected: inbox.keyState.selected === card.key

        width: parent ? parent.width : 0
        implicitHeight: body.implicitHeight + Commons.Style.spacing.lg * 2
        radius: Commons.Style.cornerRadius
        color: selected ? Commons.Style.selectedFillFor(inbox.fg, Commons.Color.accent) : Qt.rgba(inbox.fg.r, inbox.fg.g, inbox.fg.b, 0.05)

        MouseArea {
            anchors.fill: parent
            onClicked: cardItem.inbox.keyState = Object.assign({}, cardItem.inbox.keyState, { selected: cardItem.card.key })
        }

        ColumnLayout {
            id: body
            anchors.fill: parent
            anchors.margins: Commons.Style.spacing.lg
            spacing: Commons.Style.spacing.sm

            Label {
                Layout.fillWidth: true
                elide: Text.ElideRight
                color: cardItem.inbox.muted
                font.pixelSize: Commons.Style.font.bodySmall
                text: [cardItem.meta.kind, cardItem.meta.machine, cardItem.meta.project, cardItem.meta.badge].filter(x => x).join(" · ")
            }

            Label {
                Layout.fillWidth: true
                elide: Text.ElideRight
                text: cardItem.meta.title
            }

            Label {
                visible: cardItem.card.agent.pending !== null
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                maximumLineCount: 6
                text: cardItem.card.agent.pending ? cardItem.meta.tag + "  " + cardItem.card.agent.pending.detail : ""
            }

            Repeater {
                model: cardItem.rows

                delegate: Label {
                    id: optionRow
                    required property var modelData
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    color: modelData.pickable ? cardItem.inbox.fg : cardItem.inbox.muted
                    text: modelData.number + ". " + modelData.label + (modelData.rule ? "  (" + modelData.rule + ")" : "")

                    MouseArea {
                        anchors.fill: parent
                        enabled: optionRow.modelData.pickable
                        onClicked: cardItem.inbox.run({ kind: "pick", cardKey: cardItem.card.key, requestId: cardItem.card.agent.pending.id, optionId: optionRow.modelData.id, revision: cardItem.card.agent.pending.revision }, null)
                    }
                }
            }

            // Countdown: an accent bar with "Esc to undo" and a cancel control
            // that cancels the action whichever view started it (spec section 6).
            RowLayout {
                visible: cardItem.card.action !== null
                Layout.fillWidth: true

                Rectangle {
                    Layout.fillWidth: true
                    implicitHeight: Commons.Style.spacing.sm
                    color: Qt.rgba(cardItem.inbox.fg.r, cardItem.inbox.fg.g, cardItem.inbox.fg.b, 0.12)

                    Rectangle {
                        height: parent.height
                        width: cardItem.card.action ? parent.width * Model.countdownFraction(cardItem.card.action, cardItem.inbox.now) : 0
                        color: Commons.Color.accent
                    }
                }

                Label {
                    color: cardItem.inbox.muted
                    text: "Esc to undo"
                }

                Ui.Button {
                    text: "Cancel"
                    onClicked: cardItem.inbox.service.send("cancel", { action: cardItem.card.action.id }, cardItem.inbox.report)
                }
            }

            Label {
                visible: cardItem.card.notice !== null || cardItem.inbox.uncertain[cardItem.card.key] === true
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                color: Commons.Color.urgent
                text: cardItem.card.notice ? cardItem.card.notice.text : Model.UNCERTAIN_TEXT
            }

            Ui.TextField {
                id: replyField
                readonly property string draft: Model.draftKey.reply(cardItem.card)
                visible: cardItem.inbox.keyState.replyFor === cardItem.card.key
                Layout.fillWidth: true
                placeholderText: "Reply"
                Component.onCompleted: text = cardItem.inbox.drafts.get(draft)
                onVisibleChanged: if (visible) forceActiveFocus(); else cardItem.inbox.catcher.forceActiveFocus()
                onActiveFocusChanged: cardItem.inbox.focusedField = activeFocus ? replyField : (cardItem.inbox.focusedField === replyField ? null : cardItem.inbox.focusedField)
                onTextChanged: cardItem.inbox.drafts.set(draft, text)
                Keys.onPressed: event => cardItem.inbox.keyEvent(event, { kind: "reply", cardKey: cardItem.card.key }, inputMethodComposing, replyField)
            }

            Ui.TextField {
                id: answerField
                readonly property string draft: cardItem.inbox.answerEditor !== null ? Model.draftKey.answer(cardItem.inbox.answerEditor) : ""
                visible: cardItem.inbox.answerEditor !== null && cardItem.inbox.answerEditor.cardKey === cardItem.card.key
                Layout.fillWidth: true
                placeholderText: "Answer"
                Component.onCompleted: if (visible) text = cardItem.inbox.drafts.get(draft)
                onVisibleChanged: if (visible) { text = cardItem.inbox.drafts.get(draft); forceActiveFocus(); } else cardItem.inbox.catcher.forceActiveFocus()
                onActiveFocusChanged: cardItem.inbox.focusedField = activeFocus ? answerField : (cardItem.inbox.focusedField === answerField ? null : cardItem.inbox.focusedField)
                onTextChanged: if (visible && draft !== "") cardItem.inbox.drafts.set(draft, text)
                Keys.onPressed: event => cardItem.inbox.keyEvent(event, { kind: "answer", cardKey: cardItem.card.key }, inputMethodComposing, answerField)
            }
        }
    }

    Ui.PanelKeyCatcher {
        id: keyCatcher
        anchors.fill: parent
        // Typing in a field is never a shortcut.
        blocked: panel.focusedField !== null
        onCloseRequested: panel.catcherKey(Qt.Key_Escape, "", 0)
        onMoveRequested: function(dx, dy) {
            if (dy !== 0) panel.catcherKey(dy > 0 ? Qt.Key_Down : Qt.Key_Up, "", 0);
        }
        onTabRequested: function(direction) { panel.host.switchPanel(direction); }
        onTextKey: function(text, modifiers) { panel.catcherKey(0, text, modifiers); }

        ColumnLayout {
            anchors.fill: parent
            spacing: Commons.Style.spacing.lg

            RowLayout {
                Layout.fillWidth: true

                Label {
                    Layout.fillWidth: true
                    text: panel.view ? Model.headerText(panel.view) : "Inbox"
                }

                Ui.Button {
                    text: panel.settingsOpen ? "Inbox" : "Settings"
                    onClicked: panel.settingsOpen = !panel.settingsOpen
                }
            }

            Label {
                visible: !panel.service.connected
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                color: Commons.Color.urgent
                text: Model.errorText("unreachable")
            }

            Label {
                visible: panel.message !== ""
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                color: Commons.Color.urgent
                text: panel.message
            }

            // The service could not fit even the Needs-you cards (plan D9).
            Label {
                visible: !panel.settingsOpen && panel.service.overflow >= 0
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                color: Commons.Color.urgent
                text: Model.overflowText(panel.service.overflow)
            }

            Label {
                visible: !panel.settingsOpen && Model.omittedText(panel.service.omitted) !== ""
                Layout.fillWidth: true
                color: panel.muted
                text: Model.omittedText(panel.service.omitted)
            }

            Flow {
                visible: !panel.settingsOpen
                Layout.fillWidth: true
                spacing: Commons.Style.spacing.sm

                Repeater {
                    model: panel.chips

                    delegate: Ui.Button {
                        required property var modelData
                        text: modelData.name + " · " + modelData.label
                        selected: panel.machineFilter === modelData.key
                        onClicked: panel.machineFilter = selected ? "" : modelData.key
                    }
                }
            }

            Flickable {
                visible: !panel.settingsOpen
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                contentWidth: width
                contentHeight: sectionsColumn.implicitHeight

                Column {
                    id: sectionsColumn
                    width: parent.width
                    spacing: Commons.Style.spacing.lg

                    Label { visible: panel.groups.needs.length > 0; color: panel.muted; text: "Needs you" }
                    Repeater { model: panel.groups.needs; delegate: CardItem { required property var modelData; inbox: panel; card: modelData } }

                    Label { visible: panel.groups.working.length > 0; color: panel.muted; text: "Working" }
                    Repeater { model: panel.groups.working; delegate: CardItem { required property var modelData; inbox: panel; card: modelData } }

                    Label {
                        visible: panel.groups.done.length > 0
                        color: panel.muted
                        text: (panel.doneOpen ? "▾ " : "▸ ") + "Done (" + panel.groups.done.length + ")"
                        MouseArea { anchors.fill: parent; onClicked: panel.doneOpen = !panel.doneOpen }
                    }
                    Repeater { model: panel.doneOpen ? panel.groups.done : []; delegate: CardItem { required property var modelData; inbox: panel; card: modelData } }
                }
            }

            // Settings (spec section 7): machines, the three show toggles, notifications.
            ColumnLayout {
                visible: panel.settingsOpen
                Layout.fillWidth: true
                spacing: Commons.Style.spacing.lg

                Ui.Button {
                    visible: Model.keyringLocked(panel.service.settings)
                    text: "Unlock keyring"
                    onClicked: panel.service.send("unlock_secrets", {}, panel.report)
                }

                Repeater {
                    model: panel.service.settings ? panel.service.settings.machines : []

                    delegate: RowLayout {
                        required property var modelData
                        Layout.fillWidth: true

                        Ui.TextField {
                            id: nameField
                            readonly property string draft: Model.draftKey.rename(modelData.key)
                            Layout.fillWidth: true
                            Component.onCompleted: text = panel.drafts.get(draft) || modelData.name
                            onActiveFocusChanged: panel.focusedField = activeFocus ? nameField : (panel.focusedField === nameField ? null : panel.focusedField)
                            onTextChanged: panel.drafts.set(draft, text === modelData.name ? "" : text)
                            Keys.onPressed: event => panel.keyEvent(event, { kind: "rename", cardKey: null }, inputMethodComposing, nameField)
                            onAccepted: {
                                panel.drafts.clear(draft);
                                panel.service.send("rename_machine", { machine: modelData.key, name: text }, panel.report);
                            }
                        }

                        Label {
                            color: panel.muted
                            text: (panel.chips.find(c => c.key === modelData.key) ?? { label: "" }).label
                        }

                        Ui.Button {
                            text: "Retry"
                            onClicked: panel.service.send("retry_machine", { machine: modelData.key }, panel.report)
                        }

                        Ui.Button {
                            text: "Remove"
                            visible: modelData.endpoint.kind === "tcp"
                            onClicked: panel.service.send("remove_machine", { machine: modelData.key }, panel.report)
                        }
                    }
                }

                RowLayout {
                    Layout.fillWidth: true

                    Ui.TextField {
                        id: pairingField
                        Layout.fillWidth: true
                        password: true
                        placeholderText: "Paste the line from `acd pair`"
                        onActiveFocusChanged: panel.focusedField = activeFocus ? pairingField : (panel.focusedField === pairingField ? null : panel.focusedField)
                        Keys.onPressed: event => panel.keyEvent(event, { kind: "pairing", cardKey: null }, inputMethodComposing, pairingField)
                    }

                    Ui.Button {
                        text: "Add"
                        onClicked: panel.submitPairing()
                    }
                }

                Ui.Toggle {
                    Layout.fillWidth: true
                    label: "Show idle agents"
                    checked: panel.service.settings ? panel.service.settings.show_idle : false
                    onClicked: panel.service.send("set_show", { what: "idle", on: !checked }, panel.report)
                }

                Ui.Toggle {
                    Layout.fillWidth: true
                    label: "Show subagents"
                    checked: panel.service.settings ? panel.service.settings.show_subagents : false
                    onClicked: panel.service.send("set_show", { what: "subagents", on: !checked }, panel.report)
                }

                Ui.Toggle {
                    Layout.fillWidth: true
                    label: "Show hook-only agents"
                    checked: panel.service.settings ? panel.service.settings.show_hook_only : false
                    onClicked: panel.service.send("set_show", { what: "hook_only", on: !checked }, panel.report)
                }

                Ui.Toggle {
                    Layout.fillWidth: true
                    label: "Notifications"
                    checked: panel.service.settings ? panel.service.settings.notifications : true
                    onClicked: panel.service.send("set_notifications", { on: !checked }, panel.report)
                }
            }
        }
    }
}
