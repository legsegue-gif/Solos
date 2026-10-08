import XCTest
@testable import Solos

final class ChatStateTests: XCTestCase {
    private func info() -> SessionInfo {
        SessionInfo(id: "s", title: nil, model: nil, thinking: nil, agentMode: nil, createdAt: 0, updatedAt: 0, preview: nil, pinnedAt: nil)
    }

    private func snapshot(seq: UInt64, messages: [Message] = [], turn: RunningTurn? = nil) -> Snapshot {
        Snapshot(session: info(), messages: messages, turn: turn, queued: [], compactions: [], lastError: nil, seq: seq)
    }

    private func ev(_ seq: UInt64, _ kind: EventKind) -> Event {
        Event(sessionId: "s", seq: seq, kind: kind)
    }

    func testEventsApplyInOrderAndAGapAsksForASnapshot() {
        var s = ChatState(snapshot: snapshot(seq: 3))
        XCTAssertEqual(s.apply(ev(4, .turnStarted(turnId: "t"))), .applied)
        XCTAssertTrue(s.turnRunning)
        XCTAssertEqual(s.apply(ev(4, .turnStarted(turnId: "t"))), .stale)
        XCTAssertEqual(s.apply(ev(6, .messageStarted(messageId: "m"))), .gap)
        XCTAssertEqual(s.seq, 4, "a gap changes nothing")
    }

    func testStreamingIsReplacedByTheCommittedMessage() {
        var s = ChatState(snapshot: snapshot(seq: 0))
        _ = s.apply(ev(1, .turnStarted(turnId: "t")))
        _ = s.apply(ev(2, .messageStarted(messageId: "m")))
        _ = s.apply(ev(3, .thinkingDelta(messageId: "m", delta: "hm")))
        _ = s.apply(ev(4, .textDelta(messageId: "m", delta: "Hel")))
        _ = s.apply(ev(5, .textDelta(messageId: "m", delta: "lo")))
        XCTAssertEqual(s.streaming?.parts, [.thinking(text: "hm", signature: nil, redacted: nil, origin: nil), .text(text: "Hello")])
        let final = Message(id: "m", role: .assistant, parts: [.text(text: "Hello")], createdAt: 1)
        _ = s.apply(ev(6, .messageCommitted(message: final)))
        XCTAssertNil(s.streaming)
        XCTAssertEqual(s.messages, [final])
    }

    func testAToolRowFollowsItsCallFromStartToResult() {
        var s = ChatState(snapshot: snapshot(seq: 0))
        _ = s.apply(ev(1, .messageStarted(messageId: "m")))
        _ = s.apply(ev(2, .toolCallStarted(messageId: "m", callId: "c", name: "shell")))
        _ = s.apply(ev(3, .toolCallReady(messageId: "m", callId: "c", inputJson: "{\"command\":\"ls\"}", title: "List")))
        _ = s.apply(ev(4, .toolRunning(callId: "c")))
        _ = s.apply(ev(5, .toolOutputDelta(callId: "c", chunk: "a\n")))
        XCTAssertEqual(s.runningTools, ["c"])
        XCTAssertEqual(s.liveOutput["c"], "a\n")
        let result = Message(id: "r", role: .tool, parts: [.toolResult(callId: "c", output: "a", isError: false, durationMs: 5)], createdAt: 2)
        _ = s.apply(ev(6, .messageCommitted(message: result)))
        XCTAssertTrue(s.runningTools.isEmpty)
        XCTAssertNil(s.liveOutput["c"])
        XCTAssertEqual(s.result(for: "c")?.output, "a")
    }

    func testAFailedTurnKeepsItsErrorAndEndsTheTurn() {
        var s = ChatState(snapshot: snapshot(seq: 0))
        _ = s.apply(ev(1, .turnStarted(turnId: "t")))
        _ = s.apply(ev(2, .messageStarted(messageId: "m")))
        _ = s.apply(ev(3, .turnFinished(turnId: "t", outcome: .failed(error: .EmptyResponse))))
        XCTAssertFalse(s.turnRunning)
        XCTAssertNil(s.streaming)
        XCTAssertEqual(s.lastError, .EmptyResponse)
    }

    func testASnapshotTakenMidTurnShowsThePartialReply() {
        let partial = Message(id: "m", role: .assistant, parts: [.text(text: "par")], createdAt: 0)
        var s = ChatState(snapshot: snapshot(seq: 9, turn: RunningTurn(turnId: "t", streaming: partial, runningTools: [])))
        XCTAssertTrue(s.turnRunning)
        _ = s.apply(ev(10, .textDelta(messageId: "m", delta: "tial")))
        XCTAssertEqual(s.streaming?.parts, [.text(text: "partial")])
    }

    func testTruncationDropsWhatFollowsAndCanClearEverything() {
        let q = Message(id: "q", role: .user, parts: [.text(text: "q")], createdAt: 0)
        let a = Message(id: "a", role: .assistant, parts: [.text(text: "a")], createdAt: 0)
        var s = ChatState(snapshot: snapshot(seq: 0, messages: [q, a]))
        XCTAssertFalse(s.canResume)
        _ = s.apply(ev(1, .truncated(after: "q")))
        XCTAssertEqual(s.messages, [q])
        XCTAssertTrue(s.canResume, "the user spoke last and nothing runs")
        _ = s.apply(ev(2, .truncated(after: nil)))
        XCTAssertEqual(s.messages, [])
        XCTAssertFalse(s.canResume)
    }
}
