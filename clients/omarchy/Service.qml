// The socket client (omarchy spec section 7). Logic and texts live in model.mjs.
// Command fields are written to the socket and never stored or printed here.
import QtQuick
import Quickshell
import Quickshell.Io
import "model.mjs" as Model

Item {
    id: root

    property var view: null
    property var settings: null
    // Cards the service left out of a large view (plan D9).
    property var omitted: ({ done: 0, working: 0 })
    // `needs_you` of a `view_overflow` message; -1 while views fit (plan D9).
    property int overflow: -1
    property bool connected: false
    property var tracker: new Model.Tracker()
    property var ids: new Model.Ids("omarchy")
    property var reader: new Model.LineReader()
    // Reply callbacks by command id. Nothing else about a command is kept.
    property var waiting: ({})
    property var backoff: new Model.Backoff()

    signal outcome(var msg)
    signal outcomesLost(var cards)
    signal ready()

    readonly property string socketPath: Quickshell.env("XDG_RUNTIME_DIR") + "/acd-inbox.sock"

    function reconnectSoon() {
        if (retry.running) return;
        retry.interval = root.backoff.next();
        retry.start();
    }

    function dropConnection() {
        socket.connected = false;
    }

    function handle(msg) {
        if (msg === null) {
            dropConnection();
            return;
        }
        switch (msg.type) {
        case "hello":
            if (msg.protocol !== 1) dropConnection();
            else {
                // Only a valid hello clears the backoff: a service that accepts
                // and drops must not be retried at full speed.
                root.backoff.reset();
                root.ready();
            }
            break;
        case "settings":
            root.settings = msg;
            break;
        case "view":
            root.view = msg.view;
            root.omitted = msg.omitted || ({ done: 0, working: 0 });
            root.overflow = -1;
            break;
        case "view_overflow":
            root.view = null;
            root.overflow = msg.needs_you;
            break;
        case "outcome":
            root.tracker.outcome(msg);
            root.outcome(msg);
            break;
        case "outcomes_lost":
            root.outcomesLost(root.tracker.lost());
            break;
        case "ok":
        case "error": {
            const done = root.waiting[msg.id];
            delete root.waiting[msg.id];
            if (done) done(msg);
            break;
        }
        }
    }

    // Sends a command; `done(reply)` gets its ok or error. A command is never
    // sent again after a reconnect: the user acts again (spec section 5.2).
    function send(type, fields, done) {
        if (!root.connected) {
            if (done) done({ type: "error", code: "unreachable" });
            return;
        }
        const id = root.ids.next();
        if (done) root.waiting[id] = done;
        socket.write(JSON.stringify(Object.assign({}, fields, { type: type, id: id })) + "\n");
        socket.flush();
    }

    Socket {
        id: socket
        path: root.socketPath
        connected: true

        onConnectedChanged: {
            root.connected = connected;
            if (connected) {
                root.overflow = -1;
                // One reader per connection: it stays dead after an overflow.
                root.reader = new Model.LineReader();
                return;
            }
            root.view = null;
            const pending = root.waiting;
            root.waiting = ({});
            for (const id in pending) pending[id]({ type: "error", code: "unreachable" });
            root.reconnectSoon();
        }

        onError: root.reconnectSoon()

        parser: SplitParser {
            splitMarker: "\n"
            onRead: data => {
                const r = root.reader.push(data + "\n");
                for (const line of r.lines) root.handle(Model.parseMessage(line));
                if (r.overflow) root.dropConnection();
            }
        }
    }

    Timer {
        id: retry
        repeat: false
        onTriggered: socket.connected = true
    }
}
