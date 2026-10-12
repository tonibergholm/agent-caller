// Bar widget (omarchy spec section 7): a glyph and the number of agents that
// need the user. Dim when none, urgent when some, a crossed glyph when the
// service is unreachable. Click toggles the popup. The entry point is a
// Ui.Panel whose KeyboardPanel holds the popup; Task 19 fills it.
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

    // Card key from `openCard`, handed to the popup content (Task 19).
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
        anchorItem: button
        owner: root
        bar: root.bar
        open: root.opened
        focusTarget: keyCatcher

        Ui.PanelKeyCatcher {
            id: keyCatcher
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
