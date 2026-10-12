// Bar widget (omarchy spec section 7): a glyph and the number of agents that
// need the user. Dim when none, urgent when some, a crossed glyph when the
// service is unreachable. Click toggles the popup. The entry point is a
// Ui.Panel whose KeyboardPanel holds the popup content, Panel.qml.
import QtQuick
import qs.Commons as Commons
import qs.Ui as Ui
import "model.mjs" as Model

Ui.Panel {
    id: root

    // The widget declares its own IPC target below.
    manageIpc: false

    // The bar sizes the slot from the entry item's implicit size.
    implicitWidth: button.implicitWidth
    implicitHeight: button.implicitHeight

    // Card key from `openCard`; Panel.qml selects it and clears it to "".
    property string pendingCard: ""

    // `bar` is Ui.Panel's own property, so the view-model has another name.
    readonly property var tray: Model.barState(service.view, service.connected, service.overflow)

    function openCard(key) {
        root.pendingCard = key;
        root.open();
    }

    Service {
        id: service
    }

    // For Panel.qml: a binding `service: service` would name its own property.
    readonly property var inboxService: service

    Ui.WidgetButton {
        id: button
        anchors.fill: parent
        bar: root.bar
        text: Model.barLabel(root.tray)
        active: root.tray.tone === "urgent"
        dimmed: root.tray.tone !== "urgent"
        tooltipText: root.tray.tone === "down" ? "Agent inbox unreachable" : "Agent inbox"
        onPressed: function(mouseButton) { root.toggle(); }
    }

    Ui.KeyboardPanel {
        id: popup
        anchorItem: button
        owner: root
        bar: root.bar
        open: root.opened
        focusTarget: inbox.catcher
        contentWidth: popup.fittedContentWidth(Commons.Style.space(420))
        contentHeight: popup.fittedContentHeight(Commons.Style.space(560))

        Panel {
            id: inbox
            anchors.fill: parent
            service: root.inboxService
            host: root
        }
    }

    // omarchy-shell tonibergholm.agentcaller open|close|toggle|openCard <key>
    Commons.ShellIpc {
        target: "tonibergholm.agentcaller"

        function open(): void { root.open(); }
        function close(): void { root.close(); }
        function toggle(): void { root.toggle(); }
        function openCard(key: string): void { root.openCard(key); }
    }
}
