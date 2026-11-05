import QtQuick

Item {
    property bool manageIpc: true
    property var bar: null
    readonly property bool opened: false
    function open() {}
    function close() {}
    function toggle() {}
    function switchPanel(direction: int) {}
}
