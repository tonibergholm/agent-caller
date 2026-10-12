import Foundation
import XCTest

/// A one-shot async barrier: `wait` suspends until `release`, or returns at
/// once after it. Tests use it instead of sleeps. A wait that lasts longer
/// than `timeout` fails the test, naming the gate, and returns, so a missed
/// release cannot hang the run.
final class Gate: @unchecked Sendable {
    private let lock = NSLock()
    private var open = false
    private var waiters: [UUID: CheckedContinuation<Void, Never>] = [:]
    private let name: String
    private let timeout: TimeInterval

    init(_ name: String = "gate", timeout: TimeInterval = 10) {
        self.name = name
        self.timeout = timeout
    }

    func wait(file: StaticString = #filePath, line: UInt = #line) async {
        let id = UUID()
        await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in
            lock.lock()
            if open {
                lock.unlock()
                c.resume()
                return
            }
            waiters[id] = c
            lock.unlock()
            DispatchQueue.global().asyncAfter(deadline: .now() + timeout) { [self] in
                lock.lock()
                let c = waiters.removeValue(forKey: id)
                lock.unlock()
                if let c {
                    XCTFail("gate '\(name)' was never released within \(timeout) s", file: file, line: line)
                    c.resume()
                }
            }
        }
    }

    func release() {
        lock.lock()
        open = true
        let w = Array(waiters.values)
        waiters = [:]
        lock.unlock()
        w.forEach { $0.resume() }
    }
}
