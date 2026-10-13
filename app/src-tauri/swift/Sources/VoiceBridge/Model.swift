import Foundation
import VoiceBridgeC
#if canImport(FoundationModels)
import FoundationModels
#endif

/// One plan on the wire (voice spec §5.4, plan V3): Rust's `ModelPlan`, the
/// same seven fields, absent ones omitted, keys sorted.
struct PlanJSON: Encodable, Equatable {
    static let steps = ["answer", "answer_text", "send", "cancel", "read", "summarize", "select", "none"]
    var step: String
    var agent: String? = nil
    var request: String? = nil
    var option: String? = nil
    var text: String? = nil
    var machine: String? = nil
    var candidates: [String]? = nil

    func encoded() -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        // Strings and string arrays always encode; the fallback is unreachable.
        let data = (try? encoder.encode(self)) ?? Data("{\"step\":\"none\"}".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}

/// Why a plan failed, classified (spec §6.3). Never an error's text.
enum PlanFailure: Error, Equatable {
    case unavailable(UInt32)
    case timedOut
    case failed
}

/// The model behind the requests; a protocol so tests run without Apple
/// Intelligence.
protocol PlanEngine: AnyObject, Sendable {
    /// VB_MA_*. Cheap; read on every request (plan V6).
    func availability() -> UInt32
    func prewarm(instructions: String)
    /// One plan as wire JSON, from a model session no other plan used (V1).
    func plan(instructions: String, input: String) async throws -> String
    /// Drops the prewarmed session.
    func discard()
}

/// Plans in flight, by the caller's id (plan V4, V5). `lock` guards every
/// var below it. A request leaves `live` when it replies or is cancelled,
/// whichever comes first; only the first may emit.
final class ModelRequests: @unchecked Sendable {
    static let shared = ModelRequests(engine: ModelRequests.live(), bridge: .shared)

    private let engine: PlanEngine
    private let bridge: Bridge
    private let lock = NSLock()
    private var live: Set<UInt64> = []
    private var tasks: [UInt64: Task<Void, Never>] = [:]
    private var reported: UInt32?
    /// Engine calls still running, cancelled or not (review item 11b).
    private var running = 0
    static let maxRunning = 2

    init(engine: PlanEngine, bridge: Bridge) {
        self.engine = engine
        self.bridge = bridge
    }

    var countForTest: Int { lock.withLock { live.count } }
    var runningForTest: Int { lock.withLock { running } }

    private func claimSlot() -> Bool {
        lock.withLock {
            guard running < ModelRequests.maxRunning else { return false }
            running += 1
            return true
        }
    }

    private func releaseSlot() {
        lock.withLock { running -= 1 }
    }

    func start(_ id: UInt64, instructions: String, input: String, timeoutMs: UInt32) -> Int32 {
        guard id != 0 else { return VB_ERR_FAILED }
        lock.withLock { _ = live.insert(id) }
        let task = Task.detached { [self] in
            await run(id, instructions: instructions, input: input, timeoutMs: timeoutMs)
        }
        let gone = lock.withLock { () -> Bool in
            guard live.contains(id) else { return true }
            tasks[id] = task
            return false
        }
        if gone { task.cancel() }
        return VB_OK
    }

    func cancel(_ id: UInt64) {
        let task = lock.withLock { () -> Task<Void, Never>? in
            live.remove(id)
            return tasks.removeValue(forKey: id)
        }
        task?.cancel()
    }

    /// Voice off, lock, sleep, quit (spec §6.3).
    func cancelAll() {
        let all = lock.withLock { () -> [Task<Void, Never>] in
            live.removeAll()
            defer { tasks.removeAll() }
            return Array(tasks.values)
        }
        all.forEach { $0.cancel() }
        engine.discard()
    }

    func prewarm(instructions: String) {
        let code = engine.availability()
        report(code)
        if code == VB_MA_AVAILABLE { engine.prewarm(instructions: instructions) }
    }

    func query(force: Bool = false) {
        report(engine.availability(), force: force)
    }

    private func run(_ id: UInt64, instructions: String, input: String, timeoutMs: UInt32) async {
        var event = Event(generation: id, kind: VB_EV_MODEL)
        let code = engine.availability()
        report(code)
        if code != VB_MA_AVAILABLE {
            event.code = UInt32(VB_ERR_MODEL_UNAVAILABLE)
            event.value = code
        } else if !claimSlot() {
            // Earlier work that ignored its cancellation still runs: refuse
            // now rather than queue behind it (review item 11b).
            event.code = UInt32(VB_ERR_MODEL_FAILED)
        } else {
            defer { releaseSlot() } // runs on every exit, including the cancellation return
            do {
                let engine = self.engine
                event.text = try await ModelRequests.within(ms: UInt64(timeoutMs) + 1000) {
                    try await engine.plan(instructions: instructions, input: input)
                }
                event.code = UInt32(VB_OK)
            } catch is CancellationError {
                return
            } catch PlanFailure.unavailable(let why) {
                report(why)
                event.code = UInt32(VB_ERR_MODEL_UNAVAILABLE)
                event.value = why
            } catch PlanFailure.timedOut {
                event.code = UInt32(VB_ERR_MODEL_TIMEOUT)
            } catch {
                event.code = UInt32(VB_ERR_MODEL_FAILED) // never the error's text
            }
        }
        let current = lock.withLock { () -> Bool in
            tasks.removeValue(forKey: id)
            return live.remove(id) != nil
        }
        if current { bridge.emit(event) }
    }

    /// Emits VB_EV_MODEL_CAPS when the availability changed (plan V16), or
    /// always with `force`: after an overflow Rust cannot know whether the
    /// last report arrived, so it asks again with force (review item 6).
    private func report(_ code: UInt32, force: Bool = false) {
        let send = lock.withLock { () -> Bool in
            guard force || reported != code else { return false }
            reported = code
            return true
        }
        if send { bridge.emit(Event(kind: VB_EV_MODEL_CAPS, code: code)) }
    }

    static func within(ms: UInt64, _ work: @escaping @Sendable () async throws -> String) async throws -> String {
        try await withThrowingTaskGroup(of: String.self) { group in
            group.addTask { try await work() }
            group.addTask {
                try await Task.sleep(nanoseconds: ms * 1_000_000)
                throw PlanFailure.timedOut
            }
            defer { group.cancelAll() }
            guard let first = try await group.next() else { throw PlanFailure.failed }
            return first
        }
    }

    static func live() -> PlanEngine {
        #if canImport(FoundationModels)
        if #available(macOS 26.0, *) { return FoundationPlanEngine() }
        return UnavailableEngine(code: VB_MA_OS_TOO_OLD)
        #else
        return UnavailableEngine(code: VB_MA_SDK_TOO_OLD)
        #endif
    }
}

/// Before macOS 26, or built without its SDK.
final class UnavailableEngine: PlanEngine, @unchecked Sendable {
    let code: UInt32
    init(code: UInt32) { self.code = code }
    func availability() -> UInt32 { code }
    func prewarm(instructions: String) {}
    func plan(instructions: String, input: String) async throws -> String { throw PlanFailure.unavailable(code) }
    func discard() {}
}

#if canImport(FoundationModels)
/// The guided type (plan V3): the property names are the wire names.
@available(macOS 26.0, *)
@Generable
struct GuidedPlan {
    // Same list as PlanJSON.steps; PlanContractTests checks the schema has them.
    @Guide(description: "The one step for the spoken command.",
           .anyOf(["answer", "answer_text", "send", "cancel", "read", "summarize", "select", "none"]))
    var step: String
    @Guide(description: "An agent id from DATA, such as a1.")
    var agent: String?
    @Guide(description: "A request id from DATA, such as r1.")
    var request: String?
    @Guide(description: "For answer: the option's id as listed, 1 to 9.", .anyOf(["1", "2", "3", "4", "5", "6", "7", "8", "9"]))
    var option: String?
    @Guide(description: "For answer_text and send: what the user asked to say, in the user's words.")
    var text: String?
    @Guide(description: "For summarize: a machine name from DATA.")
    var machine: String?
    @Guide(description: "When unsure which agent is meant: agent ids from DATA.")
    var candidates: [String]?

    var wire: PlanJSON {
        PlanJSON(step: step, agent: agent, request: request, option: option, text: text, machine: machine, candidates: candidates)
    }
}

/// Apple's on-device model. `lock` guards `warm`.
@available(macOS 26.0, *)
final class FoundationPlanEngine: PlanEngine, @unchecked Sendable {
    private let lock = NSLock()
    private var warm: (instructions: String, session: LanguageModelSession)?

    /// The on-device model, named explicitly (review item 10): the macOS 27
    /// SDK also has a Private Cloud Compute model, and request details must
    /// never reach a cloud model by accident (spec §6.3). Never pass any
    /// other `LanguageModel`. SourceRulesTests bans the cloud type's name
    /// (so this comment spells it with spaces) and a bare default model.
    private static var onDevice: SystemLanguageModel { SystemLanguageModel.default }

    private static func session(_ instructions: String) -> LanguageModelSession {
        // `model:` typed as `SystemLanguageModel` picks the macOS 26
        // initializer, not the macOS 27 `init(model: some LanguageModel, ...)`.
        LanguageModelSession(model: onDevice, tools: [], instructions: instructions)
    }

    func availability() -> UInt32 {
        switch FoundationPlanEngine.onDevice.availability {
        case .available: return VB_MA_AVAILABLE
        case .unavailable(.deviceNotEligible): return VB_MA_DEVICE_NOT_ELIGIBLE
        case .unavailable(.appleIntelligenceNotEnabled): return VB_MA_INTELLIGENCE_OFF
        case .unavailable(.modelNotReady): return VB_MA_NOT_READY
        case .unavailable: return VB_MA_UNKNOWN
        @unknown default: return VB_MA_UNKNOWN // as Probe.swift does
        }
    }

    func prewarm(instructions: String) {
        let session = FoundationPlanEngine.session(instructions)
        session.prewarm()
        lock.withLock { warm = (instructions, session) }
    }

    func plan(instructions: String, input: String) async throws -> String {
        let prepared = lock.withLock { () -> LanguageModelSession? in
            defer { warm = nil }
            return warm?.instructions == instructions ? warm?.session : nil
        }
        let session = prepared ?? FoundationPlanEngine.session(instructions)
        let options = GenerationOptions(samplingMode: .greedy, maximumResponseTokens: 512)
        do {
            let response = try await session.respond(to: input, generating: GuidedPlan.self,
                                                     includeSchemaInPrompt: true, options: options)
            return response.content.wire.encoded()
        } catch let error as LanguageModelSession.GenerationError {
            if case .assetsUnavailable = error { throw PlanFailure.unavailable(VB_MA_NOT_READY) }
            throw PlanFailure.failed
        } catch {
            throw FoundationPlanEngine.classify(error)
        }
    }

    /// Errors from the macOS 27 runtime (review item 11a): it may throw
    /// `SystemLanguageModel.Error` or `LanguageModelError` instead of
    /// `GenerationError`. "Not ready" must still read as not ready, and a
    /// runtime timeout as a timeout. Cancellation passes through untouched.
    static func classify(_ error: Error) -> Error {
        if error is CancellationError { return error }
        // These types exist only in the macOS 27 SDK (Xcode 27, Swift 6.4).
        // Confirm the compiler version that ships with it in Step 1 and
        // adjust the guard if it differs.
        #if compiler(>=6.4)
        if #available(macOS 27.0, *) {
            if let e = error as? SystemLanguageModel.Error, case .assetsUnavailable = e {
                return PlanFailure.unavailable(VB_MA_NOT_READY)
            }
            if let e = error as? LanguageModelError, case .timeout = e {
                return PlanFailure.timedOut
            }
        }
        #endif
        return PlanFailure.failed
    }

    func discard() {
        lock.withLock { warm = nil }
    }
}
#endif

/// Borrowed UTF-8, copied before the C call returns. `nil` if not UTF-8.
func copyText(_ bytes: UnsafePointer<UInt8>?, _ count: Int) -> String? {
    guard count > 0 else { return "" }
    guard let bytes else { return nil }
    return String(bytes: UnsafeBufferPointer(start: bytes, count: count), encoding: .utf8)
}

@_cdecl("vb_model_plan")
public func vb_model_plan(_ id: UInt64, _ instructions: UnsafePointer<UInt8>?, _ instructionsLen: Int,
                          _ input: UnsafePointer<UInt8>?, _ inputLen: Int, _ timeoutMs: UInt32) -> Int32 {
    guard Bridge.shared.isInitialized else { return VB_ERR_NOT_INITIALIZED }
    guard let i = copyText(instructions, instructionsLen), let p = copyText(input, inputLen) else { return VB_ERR_FAILED }
    return ModelRequests.shared.start(id, instructions: i, input: p, timeoutMs: timeoutMs)
}

@_cdecl("vb_model_cancel")
public func vb_model_cancel(_ id: UInt64) {
    ModelRequests.shared.cancel(id)
}

@_cdecl("vb_model_cancel_all")
public func vb_model_cancel_all() {
    ModelRequests.shared.cancelAll()
}

@_cdecl("vb_model_prewarm")
public func vb_model_prewarm(_ instructions: UnsafePointer<UInt8>?, _ len: Int) {
    guard let i = copyText(instructions, len) else { return }
    Task.detached { ModelRequests.shared.prewarm(instructions: i) }
}

@_cdecl("vb_model_query")
public func vb_model_query(_ force: UInt32) {
    Task.detached { ModelRequests.shared.query(force: force != 0) }
}
