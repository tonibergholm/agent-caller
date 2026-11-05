import QtQuick

Rectangle {
    property string label: ""
    property bool checked: false
    signal clicked()
    implicitWidth: 40
    implicitHeight: 20
}
