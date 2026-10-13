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
    /// Bumped by `cancelAll`, so a prewarm that was still running during a
    /// discard does not leave a session behind (spec §6.3).
    private var epoch: UInt64 = 0
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
        // An id still in flight is refused: a second start would overwrite
        // the first task's handle, which could then no longer be cancelled.
        guard lock.withLock({ live.insert(id).inserted }) else { return VB_ERR_FAILED }
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
            epoch += 1
            live.removeAll()
            defer { tasks.removeAll() }
            return Array(tasks.values)
        }
        all.forEach { $0.cancel() }
        engine.discard()
    }

    /// The discard counter now. `vb_model_prewarm` reads it before it detaches,
    /// so a `cancelAll` that lands before the prewarm task starts still counts.
    var currentEpoch: UInt64 { lock.withLock { epoch } }

    func prewarm(instructions: String, requestedAt epoch: UInt64? = nil) {
        let code = engine.availability()
        report(code)
        guard code == VB_MA_AVAILABLE else { return }
        let started = epoch ?? currentEpoch
        engine.prewarm(instructions: instructions)
        // A discard ran while the session was being made: drop it too.
        if currentEpoch != started { engine.discard() }
    }

    func query(force: Bool = false) {
        report(engine.availability(), force: force)
    }

    private func run(_ id: UInt64, instructions: String, input: String, timeoutMs: UInt32) async {
        // Cancelled before it started (cancel or cancelAll won the race): do
        // no work. `cancel` already removed it from `live` and `tasks`.
        guard !Task.isCancelled else { return }
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
                // `cancel` and `cancelAll` took the id out of `live` first, so
                // the guard below stays silent. A cancellation nobody asked
                // for (the id is still live) is a failure, not silence.
                event.code = UInt32(VB_ERR_MODEL_FAILED)
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
        // Emitted under the lock: two reports with different codes must reach
        // Rust in the order `reported` took them (the bridge only takes its
        // own lock, so there is no cycle).
        lock.withLock {
            guard force || reported != code else { return }
            reported = code
            bridge.emit(Event(kind: VB_EV_MODEL_CAPS, code: code))
        }
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
/// The guided type (plan V3, Task 5b): one case per step, each carrying only
/// that step's own fields. A flat struct with optional fields made the model
/// fill every field (an `agent` and a `request` that pointed at different
/// cards in 18 of 20 plans), which Rust rightly refuses; a case cannot do that.
/// `wire` turns a case into Rust's `ModelPlan` JSON.
@available(macOS 26.0, *)
@Generable
enum GuidedStep {
    /// The user asks to tell, ask or message an agent something.
    case send(SendArgs)
    /// The user asks to hear a waiting request read out.
    case read(ReadArgs)
    /// The user asks what is waiting, how many things need them, or what is going on at one machine.
    case summarize(SummarizeArgs)
    /// The user asks what is waiting across everything, with no machine named.
    case summarize_all
    /// The user asks to show, open, focus or go to one agent.
    case select(SelectArgs)
    /// The user dictates a free-text answer to a question that is waiting.
    case answer_text(AnswerTextArgs)
    /// The user approves, allows, denies or picks an option for a request that is waiting.
    case answer(AnswerArgs)
    /// Only when the user asks to cancel or undo their previous voice command.
    case cancel
    /// Anything else: the command is not about the inbox or its agents.
    case none

    @Generable(description: "The user approves, allows, denies or picks an option for a request that is waiting.")
    struct AnswerArgs {
        @Guide(description: "The id of the waiting request, from DATA: r1, r2, ...")
        var request: String
        @Guide(description: "The id of the option to choose, as listed for that request: 1 to 9. Pick the option whose label or effect matches what the user said.",
               .anyOf(["1", "2", "3", "4", "5", "6", "7", "8", "9"]))
        var option: String
    }
    @Generable(description: "The user dictates a free-text answer to a question that is waiting.")
    struct AnswerTextArgs {
        @Guide(description: "The id of the waiting request, from DATA: r1, r2, ...")
        var request: String
        @Guide(description: "The answer, in the user's words.")
        var text: String
    }
    @Generable(description: "The user asks to tell, ask or message an agent something.")
    struct SendArgs {
        @Guide(description: "The id of the agent, from DATA: a1, a2, ...")
        var agent: String
        @Guide(description: "What to tell the agent, in the user's words, without the words that name the agent.")
        var text: String
    }
    @Generable(description: "The user asks to hear a waiting request read out.")
    struct ReadArgs {
        @Guide(description: "The id of the waiting request, from DATA: r1, r2, ...")
        var request: String
    }
    @Generable(description: "The user asks what is waiting, how many things need them, or what is going on at one machine.")
    struct SummarizeArgs {
        @Guide(description: "The machine's name, exactly as in DATA.")
        var machine: String
    }
    @Generable(description: "The user asks to show, open, focus or go to one agent.")
    struct SelectArgs {
        @Guide(description: "The id of the agent, from DATA: a1, a2, ...")
        var agent: String
    }

    /// Rust's `ModelPlan`: only the case's own fields are set.
    var wire: PlanJSON {
        switch self {
        case .answer(let a): return PlanJSON(step: "answer", request: a.request, option: a.option)
        case .answer_text(let a): return PlanJSON(step: "answer_text", request: a.request, text: a.text)
        case .send(let a): return PlanJSON(step: "send", agent: a.agent, text: a.text)
        case .read(let a): return PlanJSON(step: "read", request: a.request)
        case .summarize(let a): return PlanJSON(step: "summarize", machine: a.machine)
        case .summarize_all: return PlanJSON(step: "summarize")
        case .select(let a): return PlanJSON(step: "select", agent: a.agent)
        case .cancel: return PlanJSON(step: "cancel")
        case .none: return PlanJSON(step: "none")
        }
    }
}

/// The guided reply: a one-sentence restatement first (guided generation
/// decodes in property order, so the model commits to what the user wants
/// before it picks the step), then the step. `want` is dropped; Rust never
/// sees it (spec §6.3: the reply's words are not kept).
@available(macOS 26.0, *)
@Generable
struct GuidedPlan {
    @Guide(description: "One short sentence: the verb the user said and which agent or request it is about.")
    var want: String
    var step: GuidedStep

    var wire: PlanJSON { step.wire }
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

    /// This interface's own wording, added to Rust's instructions: the model
    /// sees one case per step and a summary with no machine is its own case.
    /// There is no `candidates` field in this interface (the wire format keeps
    /// it for models that offer it): the model picks the most likely agent and
    /// Rust, which resolves names itself, asks which one when the transcript is
    /// ambiguous. The cases without a payload (`summarize_all`, `cancel`,
    /// `none`) cannot carry a description in the schema, so what they mean is
    /// said here and in Rust's instructions.
    private static let interfaceNote =
        "In this interface summarize always takes a machine and is only for a machine the user named. "
        + "Use summarize_all for \"what needs me\", \"how many things are waiting\" and \"give me a summary\"."

    private static func session(_ instructions: String) -> LanguageModelSession {
        // `model:` typed as `SystemLanguageModel` picks the macOS 26
        // initializer, not the macOS 27 `init(model: some LanguageModel, ...)`.
        LanguageModelSession(model: onDevice, tools: [], instructions: instructions + "\n" + interfaceNote)
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
        // Xcode 27 renamed the label (`sampling:` stays as a deprecated alias
        // there); the CI runner's older SDK has only `sampling:`.
        #if compiler(>=6.4)
        let options = GenerationOptions(samplingMode: .greedy, maximumResponseTokens: 512)
        #else
        let options = GenerationOptions(sampling: .greedy, maximumResponseTokens: 512)
        #endif
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
    let requestedAt = ModelRequests.shared.currentEpoch
    Task.detached { ModelRequests.shared.prewarm(instructions: i, requestedAt: requestedAt) }
}

@_cdecl("vb_model_query")
public func vb_model_query(_ force: UInt32) {
    Task.detached { ModelRequests.shared.query(force: force != 0) }
}
