import Foundation
import Observation

/// The app's connection to the core: opens it, keeps the session list, and
/// hands each event to the chat that shows its session.
@MainActor
@Observable
final class AppCore {
    private(set) var core: SolosCore?
    private(set) var startupError: CoreError?
    private(set) var sessions: [SessionInfo] = []
    /// Set by a chat that wants a fresh one opened in its place.
    var newChatRequested = false
    /// A chat to open, from a tapped notification.
    var openSessionRequested: String?
    @ObservationIgnored private lazy var lifecycle: TurnLifecycle = {
        let l = TurnLifecycle()
        l.openSession = { [weak self] id in self?.openSessionRequested = id }
        return l
    }()
    private(set) var settings = Settings(endpoints: [], defaultModel: nil, thinking: false, agentMode: true)

    /// Open chats by session id. Weak, so a closed chat simply stops
    /// receiving.
    @ObservationIgnored private var chats: [String: WeakChat] = [:]

    func start() async {
        guard core == nil else { return }
        let fm = FileManager.default
        let dataDir = fm.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("Solos")
        try? fm.createDirectory(at: dataDir, withIntermediateDirectories: true)
        let rootfs = Bundle.main.path(forResource: "alpine-rootfs", ofType: "zip")
        let capture = ProcessInfo.processInfo.environment["SOLOS_CAPTURE_DIR"]
        do {
            let core = try await openCore(
                config: CoreConfig(dataDir: dataDir.path, bundledRootfs: rootfs, captureDir: capture),
                secrets: KeychainSecrets(),
                browser: BrowserTools(),
                device: DeviceTools())
            _ = lifecycle
            core.subscribe(sink: EventRelay(self))
            self.core = core
            DownloadCenter.shared.workspace = URL(fileURLWithPath: core.workspaceDir())
            settings = core.settings()
            #if DEBUG
            // Testing the context limits on screen without filling a real
            // model's window: SOLOS_TEST_WINDOW sets the default model's.
            if let window = ProcessInfo.processInfo.environment["SOLOS_TEST_WINDOW"].flatMap(UInt64.init),
               let model = settings.defaultModel {
                try? await core.setModelWindow(model: model, window: window)
            }
            #endif
            await reloadSessions()
            // Boot early so the first command does not wait for it. A freshly
            // unpacked system then gets the fastest package mirrors, as the
            // reference app's first launch does; nothing happens otherwise.
            Task {
                try? await core.bootSandbox()
                _ = try? await core.choosePackageMirrorsIfFresh()
            }
        } catch let e as CoreError {
            startupError = e
        } catch {
            startupError = .Internal(detail: String(describing: error))
        }
    }

    func reloadSessions() async {
        guard let core else { return }
        sessions = (try? await core.listSessions()) ?? sessions
    }

    func save(_ new: Settings) async throws {
        try await core?.setSettings(settings: new)
        settings = new
    }

    /// Remove endpoints and their keys. The default model goes with its
    /// endpoint; chats that used one keep their messages and need another model.
    func removeEndpoints(_ removed: [Endpoint]) async throws {
        var s = settings
        s.endpoints.removeAll { e in removed.contains { $0.id == e.id } }
        if let d = s.defaultModel, removed.contains(where: { $0.id == d.endpointId }) {
            s.defaultModel = nil
        }
        try await save(s)
        removed.forEach { Keychain.write(EndpointEditor.secretRef(for: $0), "") }
    }

    /// The file on this device a workspace link (`solos://ws/…`) names, if
    /// it exists.
    func localFile(_ reference: String) -> URL? {
        guard let path = core?.resolveFile(reference: reference),
              FileManager.default.fileExists(atPath: path) else { return nil }
        return URL(fileURLWithPath: path)
    }

    func register(_ chat: ChatModel, for sessionId: String) {
        chats[sessionId] = WeakChat(chat)
    }

    fileprivate func deliver(_ event: Event) {
        chats[event.sessionId]?.chat?.receive(event)
        lifecycle.handle(event, title: self.sessions.first { $0.id == event.sessionId }?.title)
        if case .sessionUpdated = event.kind {
            Task { await reloadSessions() }
        }
    }

    fileprivate func resyncAll() {
        for c in chats.values { c.chat?.resync() }
        Task { await reloadSessions() }
    }
}

private struct WeakChat {
    weak var chat: ChatModel?
    init(_ chat: ChatModel) { self.chat = chat }
}

/// Receives events on a core thread and hands them to the main thread in the
/// order they came. `DispatchQueue.main.async` is FIFO, which a `Task` per
/// event is not.
private final class EventRelay: EventSink, @unchecked Sendable {
    private weak var app: AppCore?
    init(_ app: AppCore) { self.app = app }

    func onEvent(event: Event) {
        DispatchQueue.main.async { [weak app] in
            MainActor.assumeIsolated { app?.deliver(event) }
        }
    }

    func onResync() {
        DispatchQueue.main.async { [weak app] in
            MainActor.assumeIsolated { app?.resyncAll() }
        }
    }
}
