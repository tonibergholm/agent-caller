import Foundation

/// A one-shot async barrier: `wait` suspends until `release`, or returns at
/// once after it. Tests use it instead of sleeps.
final class Gate: @unchecked Sendable {
    private let lock = NSLock()
    private var open = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in
            lock.lock()
            if open {
                lock.unlock()
                c.resume()
            } else {
                waiters.append(c)
                lock.unlock()
            }
        }
    }

    func release() {
        lock.lock()
        open = true
        let w = waiters
        waiters = []
        lock.unlock()
        w.forEach { $0.resume() }
    }
}
