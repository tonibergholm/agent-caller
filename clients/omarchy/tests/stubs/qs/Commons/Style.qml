pragma Singleton
import QtQuick

// Test stub of the shell's metrics.
QtObject {
    readonly property real cornerRadius: 4
    readonly property var spacing: ({ sm: 4, lg: 8 })
    readonly property var font: ({ family: "sans", body: 12, bodySmall: 10 })
    function space(n: real): real { return n; }
    function selectedFillFor(fg: color, accent: color): color { return accent; }
}
