import Foundation

/// Everything the chat screen shows for one session, derived purely from a
/// snapshot and the events after it. No UI, no I/O: unit-tested directly.
struct ChatState: Equatable {
    var session: SessionInfo?
    var messages: [Message] = []
    /// The assistant message arriving right now, as far as it has got.
    var streaming: Message?
    var runningTools: Set<String> = []
    /// Live output of running tools, by call id. Not in snapshots: it is only
    /// a preview until the result is committed.
    var liveOutput: [String: String] = [:]
    var queued: [String] = []
    var compactions: [Compaction] = []
    var turnRunning = false
    var retrying: CoreError?
    var lastError: CoreError?
    /// The last event reflected here.
    var seq: UInt64 = 0

    enum Applied: Equatable {
        case applied
        /// Already reflected (an event from before the snapshot).
        case stale
        /// Events are missing: fetch a snapshot and start over.
        case gap
    }

    /// Live output kept per tool call, so a noisy command cannot grow memory
    /// without bound. The committed result is the record; this is a preview.
    static let liveOutputLimit = 64_000

    init() {}

    init(snapshot: Snapshot) {
        load(snapshot)
    }

    mutating func load(_ s: Snapshot) {
        session = s.session
        messages = s.messages
        streaming = s.turn?.streaming
        runningTools = Set(s.turn?.runningTools ?? [])
        liveOutput = [:]
        queued = s.queued
        compactions = s.compactions
        lastError = s.lastError
        turnRunning = s.turn != nil
        retrying = nil
        seq = s.seq
    }

    mutating func apply(_ event: Event) -> Applied {
        if event.seq <= seq { return .stale }
        if event.seq != seq + 1 { return .gap }
        seq = event.seq
        reduce(event.kind)
        return .applied
    }

    private mutating func reduce(_ kind: EventKind) {
        switch kind {
        case .turnStarted:
            turnRunning = true
            lastError = nil
            retrying = nil
        case .messageStarted(let id):
            streaming = Message(id: id, role: .assistant, parts: [], createdAt: 0)
            retrying = nil
        case .textDelta(_, let delta):
            appendText(delta, thinking: false)
        case .thinkingDelta(_, let delta):
            appendText(delta, thinking: true)
        case .toolCallStarted(_, let callId, let name):
            streaming?.parts.append(.toolCall(id: callId, name: name, inputJson: "", title: nil, signature: nil))
        case .toolCallReady(_, let callId, let inputJson, let title):
            guard var m = streaming else { return }
            m.parts = m.parts.map { part in
                if case .toolCall(let id, let name, _, _, let signature) = part, id == callId {
                    return .toolCall(id: id, name: name, inputJson: inputJson, title: title, signature: signature)
                }
                return part
            }
            streaming = m
        case .toolRunning(let callId):
            runningTools.insert(callId)
        case .toolOutputDelta(let callId, let chunk):
            var text = liveOutput[callId, default: ""] + chunk
            if text.count > Self.liveOutputLimit { text = String(text.suffix(Self.liveOutputLimit)) }
            liveOutput[callId] = text
        case .messageCommitted(let message):
            if message.role == .assistant, streaming?.id == message.id { streaming = nil }
            if message.role == .tool {
                for case .toolResult(let callId, _, _, _) in message.parts {
                    runningTools.remove(callId)
                    liveOutput[callId] = nil
                }
            }
            if let i = messages.firstIndex(where: { $0.id == message.id }) {
                messages[i] = message
            } else {
                messages.append(message)
            }
        case .queued(let q):
            queued = q
        case .retrying(_, let reason):
            retrying = reason
        case .turnFinished(_, let outcome):
            turnRunning = false
            streaming = nil
            runningTools = []
            retrying = nil
            if case .failed(let error) = outcome { lastError = error }
        case .sessionUpdated(let info):
            session = info
        case .compactionsChanged(let list):
            compactions = list
            if case .ContextNearlyFull = lastError { lastError = nil }
        case .truncated(let after):
            if let after, let i = messages.firstIndex(where: { $0.id == after }) {
                messages.removeSubrange((i + 1)...)
            } else if after == nil {
                messages = []
            }
            lastError = nil
        }
    }

    private mutating func appendText(_ delta: String, thinking: Bool) {
        guard var m = streaming else { return }
        switch (m.parts.last, thinking) {
        case (.text(let t), false): m.parts[m.parts.count - 1] = .text(text: t + delta)
        case (.thinking(let t, let signature, let redacted, let origin), true):
            m.parts[m.parts.count - 1] = .thinking(text: t + delta, signature: signature, redacted: redacted, origin: origin)
        case (_, false): m.parts.append(.text(text: delta))
        case (_, true): m.parts.append(.thinking(text: delta, signature: nil, redacted: nil, origin: nil))
        }
        streaming = m
    }

    /// A turn was cut off before the model answered: the user or a tool
    /// spoke last and nothing is running, so the model can carry on.
    var canResume: Bool {
        !turnRunning && messages.last.map { $0.role != .assistant } == true
    }

    /// Summaries in force or undone, by the message they end after.
    func compaction(after messageId: String) -> Compaction? {
        compactions.last { $0.throughMessageId == messageId }
    }

    /// The result of a tool call, wherever it is in the transcript.
    func result(for callId: String) -> (output: String, isError: Bool, durationMs: UInt64?)? {
        for m in messages.reversed() where m.role == .tool {
            for case .toolResult(let id, let output, let isError, let duration) in m.parts where id == callId {
                return (output, isError, duration)
            }
        }
        return nil
    }
}
