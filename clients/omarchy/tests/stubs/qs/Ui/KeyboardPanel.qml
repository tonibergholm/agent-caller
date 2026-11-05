import QtQuick

Item {
    property Item anchorItem: null
    property Item owner: null
    property var bar: null
    property bool open: false
    property Item focusTarget: null
    property real contentWidth: 0
    property real contentHeight: 0
    function fittedContentWidth(w: real): real { return w; }
    function fittedContentHeight(h: real): real { return h; }
}
