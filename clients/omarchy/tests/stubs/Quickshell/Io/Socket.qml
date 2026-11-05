import QtQuick

Item {
    property string path: ""
    property bool connected: false
    property var parser: null
    signal error()
    function write(s: string) {}
    function flush() {}
}
