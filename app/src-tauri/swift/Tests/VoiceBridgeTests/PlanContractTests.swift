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
    func testTheGuidedTypeHasTheContractFieldsAndSteps() throws {
        guard #available(macOS 26.0, *) else { throw XCTSkip("needs macOS 26") }
        let guided = GuidedPlan(step: "answer_text", agent: "a2", request: "r1", option: "3",
                                text: "run the tests", machine: "minos", candidates: ["a1", "a2"])
        XCTAssertEqual(guided.wire, full)
        let schema = String(decoding: try JSONEncoder().encode(GuidedPlan.generationSchema), as: UTF8.self)
        // Inspect `schema` once when this first runs; if the field names are not
        // quoted keys in it, assert on the shape it has and note it in the PR.
        for name in ["step", "agent", "request", "option", "text", "machine", "candidates"] + PlanJSON.steps {
            XCTAssertTrue(schema.contains("\"\(name)\""), name)
        }
    }
    #endif
}
