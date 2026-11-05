import QtQuick

Item {
    property var bar: null
    property string text: ""
    property bool active: false
    property bool dimmed: false
    property string tooltipText: ""
    signal pressed(var mouseButton)
}
