import Foundation
import UIKit
import Observation

/// One open chat: its state, and the calls it makes.
///
/// Events are applied through `ChatState`. Text arrives far faster than the
/// screen needs it, so bursts of deltas are applied together at most 25 times
/// a second; any other event applies everything pending at once, keeping the
/// order.
@MainActor
@Observable
final class ChatModel {
    private(set) var state = ChatState()
    private(set) var sessionId: String?
    private(set) var sendError: CoreError?

    @ObservationIgnored private let app: AppCore
    @ObservationIgnored private var pending: [Event] = []
    @ObservationIgnored private var flushScheduled = false
    @ObservationIgnored private var resyncing = false

    init(app: AppCore) {
        self.app = app
    }

    /// Open an existing session, or a new one when `sessionId` is nil.
    func open(_ sessionId: String?) async {
        guard let core = app.core else { return }
        do {
            let id: String
            if let sessionId { id = sessionId } else { id = try await core.createSession(model: nil).id }
            self.sessionId = id
            app.register(self, for: id)
            state = ChatState(snapshot: try await core.snapshot(sessionId: id))
        } catch let e as CoreError {
            sendError = e
        } catch {}
    }

    /// Send text and attachments. The core copies the files before it
    /// returns, so the temporary copies go once it has.
    func send(_ text: String, attachments: [PendingAttachment] = []) async -> Bool {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty || !attachments.isEmpty else { return false }
        let ok = await call { core, id in
            try await core.send(sessionId: id, text: text, attachments: attachments.map(\.source))
        }
        if ok { attachments.forEach { $0.discard() } }
        return ok
    }

    /// Answer again from `messageId` (the last user message when nil),
    /// with its text replaced when `text` is given.
    func retry(from messageId: String?, text: String? = nil) async -> Bool {
        await call { core, id in try await core.retry(sessionId: id, messageId: messageId, text: text) }
    }

    func resume() async {
        _ = await call { core, id in try await core.resume(sessionId: id) }
    }

    private(set) var compacting = false

    /// Summarise the start of the conversation, then — when a turn stopped
    /// for room — carry on with it.
    func compact(thenContinue: Bool) async {
        compacting = true
        defer { compacting = false }
        let ok = await call { core, id in _ = try await core.compact(sessionId: id) }
        if ok && thenContinue { await resume() }
    }

    func undoCompaction(_ id: String) async {
        _ = await call { core, session in try await core.undoCompaction(sessionId: session, compactionId: id) }
    }

    func clear() async {
        _ = await call { core, id in try await core.clearSession(sessionId: id) }
    }

    /// The session as JSON on the clipboard, for reporting a problem.
    func copySessionData() async {
        _ = await call { core, id in UIPasteboard.general.string = try await core.exportSession(sessionId: id) }
    }

    /// Run a command on this session; a failure is shown where send errors are.
    private func call(_ body: (SolosCore, String) async throws -> Void) async -> Bool {
        guard let core = app.core, let sessionId else { return false }
        do {
            try await body(core, sessionId)
            sendError = nil
            return true
        } catch let e as CoreError {
            sendError = e
            return false
        } catch {
            return false
        }
    }

    func stop() {
        guard let core = app.core, let sessionId else { return }
        Task { try? await core.cancel(sessionId: sessionId) }
    }

    // MARK: - Events

    func receive(_ event: Event) {
        pending.append(event)
        switch event.kind {
        case .textDelta, .thinkingDelta, .toolOutputDelta:
            guard !flushScheduled else { return }
            flushScheduled = true
            Task { @MainActor [weak self] in
                try? await Task.sleep(for: .milliseconds(40))
                self?.flush()
            }
        default:
            flush()
        }
    }

    private func flush() {
        flushScheduled = false
        let batch = pending
        pending.removeAll()
        var next = state
        for e in batch where next.apply(e) == .gap {
            resync()
            return
        }
        state = next
    }

    /// Start over from a snapshot — the one recovery path for anything missed.
    func resync() {
        guard !resyncing, let core = app.core, let sessionId else { return }
        resyncing = true
        pending.removeAll()
        Task { @MainActor [weak self] in
            defer { self?.resyncing = false }
            guard let snap = try? await core.snapshot(sessionId: sessionId) else { return }
            self?.state = ChatState(snapshot: snap)
        }
    }
}
