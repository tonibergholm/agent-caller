import QtQuick

Item {
    property bool blocked: false
    signal closeRequested()
    signal moveRequested(int dx, int dy)
    signal tabRequested(int direction)
    signal textKey(string text, int modifiers)
}
