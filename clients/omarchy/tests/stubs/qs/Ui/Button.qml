import QtQuick

// Test stub: a clickable item. `clicked()` is emitted by tests via click().
Rectangle {
    id: root
    property string text: ""
    property bool selected: false
    signal clicked()
    implicitWidth: 40
    implicitHeight: 20
    MouseArea {
        anchors.fill: parent
        onClicked: root.clicked()
    }
}
