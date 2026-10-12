// Bar widget (omarchy spec section 7): a glyph and the number of agents that
// need the user. Muted when none, urgent when some, struck through when the
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

    // Card key from `openCard`, handed to the popup content (Task 19).
    property string pendingCard: ""

    readonly property var bar: Model.barState(service.view, service.connected)

    function openCard(key) {
        root.pendingCard = key;
        root.open();
    }

    Service {
        id: service
    }

    Item {
        id: button
        implicitWidth: row.implicitWidth + Commons.Style.spacing.sm * 2
        implicitHeight: row.implicitHeight

        Row {
            id: row
            anchors.centerIn: parent
            spacing: Commons.Style.spacing.xs

            Text {
                text: "◉"
                font.strikeout: root.bar.tone === "down"
                color: root.bar.tone === "urgent" ? Commons.Color.urgent : Commons.Color.muted
            }

            Text {
                visible: root.bar.count > 0
                text: root.bar.text
                color: Commons.Color.urgent
            }
        }

        MouseArea {
            anchors.fill: parent
            onClicked: root.toggle()
        }
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
