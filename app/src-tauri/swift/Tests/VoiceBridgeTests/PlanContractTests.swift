import XCTest
@testable import VoiceBridge
#if canImport(FoundationModels)
import FoundationModels
#endif

/// Plan V3. Rust checks the same files (crates/acd-voice/tests/plan_contract.rs).
final class PlanContractTests: XCTestCase {
    private func fixture(_ name: String) throws -> String {
        var url = URL(fileURLWithPath: #filePath)
        for _ in 0..<6 { url.deleteLastPathComponent() } // up to the repository root
        let text = try String(contentsOf: url.appendingPathComponent("tests/fixtures/voice/\(name)"), encoding: .utf8)
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private let full = PlanJSON(step: "answer_text", agent: "a2", request: "r1", option: "3",
                                text: "run the tests", machine: "minos", candidates: ["a1", "a2"])

    func testTheWireJsonMatchesTheFixtures() throws {
        XCTAssertEqual(full.encoded(), try fixture("plan-full.json"))
        XCTAssertEqual(PlanJSON(step: "none").encoded(), try fixture("plan-min.json"))
    }

    func testTheStepNamesMatchTheFixture() throws {
        let steps = try JSONDecoder().decode([String].self, from: Data(try fixture("plan-steps.json").utf8))
        XCTAssertEqual(PlanJSON.steps, steps)
    }

    #if canImport(FoundationModels)
    /// Task 5b: one guided case per step, each writing exactly the JSON in the
    /// fixture, which Rust checks against its schema and decoder.
    func testEveryGuidedCaseWritesTheWireJsonInTheFixture() throws {
        guard #available(macOS 26.0, *) else { throw XCTSkip("needs macOS 26") }
        let expected = try JSONDecoder().decode([String].self, from: Data(try fixture("plan-cases.json").utf8))
        let cases: [GuidedStep] = [
            .answer(.init(request: "r1", option: "2")),
            .answer_text(.init(request: "r1", text: "use the blue one")),
            .send(.init(agent: "a2", text: "run the tests")),
            .read(.init(request: "r1")),
            .summarize(.init(machine: "minos")),
            .summarize_all,
            .select(.init(agent: "a2")),
            .cancel,
            .none,
        ]
        XCTAssertEqual(cases.map { GuidedPlan(want: "w", step: $0).wire.encoded() }, expected)
    }

    func testTheGuidedSchemaNamesEveryFieldAndCase() throws {
        guard #available(macOS 26.0, *) else { throw XCTSkip("needs macOS 26") }
        let schema = String(decoding: try JSONEncoder().encode(GuidedPlan.generationSchema), as: UTF8.self)
        for name in ["want", "step", "agent", "request", "option", "text", "machine",
                     "answer", "answer_text", "send", "read", "summarize", "summarize_all", "select", "cancel", "none"] {
            XCTAssertTrue(schema.contains("\"\(name)\""), name)
        }
    }

    /// The restatement is the model's own scratch space: it never reaches Rust.
    func testTheRestatementIsNotPartOfTheWireJson() throws {
        guard #available(macOS 26.0, *) else { throw XCTSkip("needs macOS 26") }
        XCTAssertFalse(GuidedPlan(want: "SECRET-WORDS", step: .none).wire.encoded().contains("SECRET"))
    }
    #endif
}
