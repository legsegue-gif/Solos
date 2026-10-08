import SwiftUI

/// Runs one real turn through the real core, sandbox and platform code, with
/// no UI, then exits. For checking behaviour in the simulator from a script:
///
///     SIMCTL_CHILD_SOLOS_PROBE_BASE_URL=https://api.example.com/v1 \
///     SIMCTL_CHILD_SOLOS_PROBE_KEY=... SIMCTL_CHILD_SOLOS_PROBE_MODEL=... \
///     SIMCTL_CHILD_SOLOS_PROBE_PROMPT='...' \
///     xcrun simctl launch --console --terminate-running-process <udid> <bundle id>.probe
///
/// Prints `PROBE <key>: <value>` lines and exits 0 when the turn completed.
/// `SOLOS_PROBE_CAPTURE` names a directory for request bodies and raw streams;
/// `SOLOS_PROBE_THINKING=0` turns thinking off; `SOLOS_PROBE_WINDOW` sets the
/// model's context window; prompts separated by a `---` line run in turn,
/// each padded with `SOLOS_PROBE_FILL` characters.
@main
struct ProbeApp: App {
    var body: some Scene {
        WindowGroup {
            Text("Solos probe").task { await Probe.run() }
        }
    }
}

private final class EnvSecret: SecretStore {
    func secret(reference: String) -> String? { ProcessInfo.processInfo.environment["SOLOS_PROBE_KEY"] }
    /// The probe keeps nothing between runs; MCP values stay in its settings.
    func store(reference: String, value: String) -> Bool { false }
}

private final class Collector: EventSink, @unchecked Sendable {
    private let lock = NSLock()
    private var events: [Event] = []
    private var done: CheckedContinuation<[Event], Never>?
    private var finished = false

    func onEvent(event: Event) {
        lock.lock()
        events.append(event)
        let isEnd: Bool = if case .turnFinished = event.kind { true } else { false }
        let cont = isEnd ? done : nil
        if isEnd { finished = true; done = nil }
        let all = events
        lock.unlock()
        cont?.resume(returning: all)
    }

    func onResync() {}

    /// Start collecting the next turn.
    func reset() {
        lock.lock()
        events = []
        finished = false
        lock.unlock()
    }

    func wait() async -> [Event] {
        await withCheckedContinuation { c in
            lock.lock()
            if finished { let all = events; lock.unlock(); c.resume(returning: all); return }
            done = c
            lock.unlock()
        }
    }
}

/// Collects one terminal's output for the terminal check.
private final class TerminalCollector: TerminalSink, @unchecked Sendable {
    private let lock = NSLock()
    private var bytes = Data()
    private(set) var exitCode: Int32??
    /// Answers what a terminal is asked. ash asks where the cursor is
    /// (`ESC[6n`) after every prompt and waits for the answer.
    var answer: ((Data) -> Void)?

    var text: String {
        lock.lock(); defer { lock.unlock() }
        return String(decoding: bytes, as: UTF8.self)
    }

    var ended: Bool {
        lock.lock(); defer { lock.unlock() }
        return exitCode != nil
    }

    func output(data: Data) {
        lock.lock(); bytes.append(data); let answer = self.answer; lock.unlock()
        let asks = data.split(separator: 0x1B, omittingEmptySubsequences: false).dropFirst().filter { $0.starts(with: Data("[6n".utf8)) }.count
        for _ in 0..<asks { answer?(Data("\u{1B}[1;15R".utf8)) }
    }

    func exited(code: Int32?) {
        lock.lock(); exitCode = .some(code); lock.unlock()
    }
}

enum Probe {
    static func say(_ key: String, _ value: String) {
        print("PROBE \(key): \(value.replacingOccurrences(of: "\n", with: "\\n"))")
        fflush(stdout)
    }

    static func run() async {
        let env = ProcessInfo.processInfo.environment
        if env["SOLOS_PROBE_TERMINAL"] == "1" {
            await terminal()
            return
        }
        if let command = env["SOLOS_PROBE_SHELL"] {
            await shell(command, seconds: Double(env["SOLOS_PROBE_SHELL_WAIT"] ?? "") ?? 300)
            return
        }
        guard let base = env["SOLOS_PROBE_BASE_URL"], let model = env["SOLOS_PROBE_MODEL"], let prompt = env["SOLOS_PROBE_PROMPT"] else {
            say("error", "set SOLOS_PROBE_BASE_URL, SOLOS_PROBE_KEY, SOLOS_PROBE_MODEL and SOLOS_PROBE_PROMPT")
            exit(2)
        }
        let dataDir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("SolosProbe")
        try? FileManager.default.createDirectory(at: dataDir, withIntermediateDirectories: true)
        do {
            let core = try await openCore(
                config: CoreConfig(dataDir: dataDir.path,
                                   bundledRootfs: Bundle.main.path(forResource: "alpine-rootfs", ofType: "zip"),
                                   captureDir: env["SOLOS_PROBE_CAPTURE"]),
                secrets: EnvSecret(),
                browser: BrowserTools(),
                device: DeviceTools())
            let started = Date()
            try await core.bootSandbox()
            say("boot_ms", "\(Int(Date().timeIntervalSince(started) * 1000))")
            try await core.setSettings(settings: Settings(
                endpoints: [Endpoint(id: "probe", name: "probe", protocol: .openAi, baseUrl: base, secretRef: "probe")],
                defaultModel: ModelChoice(endpointId: "probe", model: model),
                thinking: env["SOLOS_PROBE_THINKING"] != "0"))
            let collector = Collector()
            core.subscribe(sink: collector)
            if let window = env["SOLOS_PROBE_WINDOW"].flatMap(UInt64.init) {
                try await core.setModelWindow(model: ModelChoice(endpointId: "probe", model: model), window: window)
            }
            let session = try await core.createSession(model: nil)
            say("session", session.id)
            // Several prompts, separated by a line of ---, each padded with
            // SOLOS_PROBE_FILL characters to fill the window quickly.
            let filler = String(repeating: "lorem ", count: (env["SOLOS_PROBE_FILL"].flatMap(Int.init) ?? 0) / 6)
            var events: [Event] = []
            for (i, text) in prompt.components(separatedBy: "\n---\n").enumerated() {
                collector.reset()
                let turnStarted = Date()
                try await core.send(sessionId: session.id, text: filler.isEmpty ? text : "\(text)\n\n\(filler)", attachments: [])
                events = await collector.wait()
                say("turn_ms", "\(i): \(Int(Date().timeIntervalSince(turnStarted) * 1000))")
                report(events)
                // Out of room: summarise, as the user would agree to, and carry on.
                let full = events.contains { if case .turnFinished(_, .failed(.ContextNearlyFull(_, _, true))) = $0.kind { true } else { false } }
                if full {
                    let c = try await core.compact(sessionId: session.id)
                    say("summary", "\(c.summary.count) chars through \(c.throughMessageId): \(c.summary.prefix(300))")
                    collector.reset()
                    try await core.resume(sessionId: session.id)
                    events = await collector.wait()
                    say("after_summary", "")
                    report(events)
                }
            }
            say("title", await title(core, session.id) ?? "(none after 60s)")
            let completed = events.contains { if case .turnFinished(_, .completed) = $0.kind { true } else { false } }
            exit(completed ? 0 : 1)
        } catch {
            say("error", String(describing: error))
            exit(1)
        }
    }

    /// The session's title, which the core writes in the background after
    /// the first reply.
    static func title(_ core: SolosCore, _ id: String) async -> String? {
        for _ in 0..<120 {
            if let t = try? await core.snapshot(sessionId: id).session.title { return t }
            try? await Task.sleep(for: .milliseconds(500))
        }
        return nil
    }

    static func report(_ events: [Event]) {
        var tools: [String] = []
        var answer = ""
        for e in events {
            switch e.kind {
            case .messageCommitted(let m) where m.role == .assistant:
                for p in m.parts {
                    switch p {
                    case .toolCall(_, let name, let input, _, _): tools.append("\(name) \(input)")
                    case .text(let t): answer = t
                    default: break
                    }
                }
            case .messageCommitted(let m) where m.role == .tool:
                for case .toolResult(_, let output, let isError, let ms) in m.parts {
                    say("tool_result", "\(isError ? "ERROR " : "")\(ms.map { "\($0)ms " } ?? "")\(output.prefix(300))")
                }
            case .turnFinished(_, let outcome):
                say("outcome", String(describing: outcome))
            default:
                break
            }
        }
        say("tools", tools.joined(separator: " | "))
        say("answer", answer)
        let seqs = events.map(\.seq)
        say("events", "\(events.count), gapless: \(seqs == Array(1...UInt64(max(seqs.count, 1))).prefix(seqs.count).map { $0 })")
    }

    /// `SOLOS_PROBE_SHELL='<command>'`: runs one command in the guest's login
    /// shell and prints its output and how long it took, for questions about
    /// the sandbox itself ("does wget finish this download?"). Waits up to
    /// `SOLOS_PROBE_SHELL_WAIT` seconds (default 300). `SOLOS_PROBE_MIRRORS`
    /// chooses package mirrors first.
    static func shell(_ command: String, seconds: Double) async {
        let dataDir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("SolosProbe")
        try? FileManager.default.createDirectory(at: dataDir, withIntermediateDirectories: true)
        do {
            let core = try await openCore(
                config: CoreConfig(dataDir: dataDir.path,
                                   bundledRootfs: Bundle.main.path(forResource: "alpine-rootfs", ofType: "zip"),
                                   captureDir: nil),
                secrets: EnvSecret(),
                browser: BrowserTools(),
                device: DeviceTools())
            try await core.bootSandbox()
            // `SOLOS_PROBE_MIRRORS=alpine=aliyun,pip=tuna,npm=npmmirror`:
            // chosen through the core, as Settings does, before the command.
            for pair in (ProcessInfo.processInfo.environment["SOLOS_PROBE_MIRRORS"] ?? "").split(separator: ",") {
                let kv = pair.split(separator: "=").map(String.init)
                let kinds: [String: MirrorKind] = ["alpine": .alpine, "pip": .pip, "npm": .npm]
                guard kv.count == 2, let kind = kinds[kv[0]] else { continue }
                try await core.setPackageMirror(kind: kind, id: kv[1])
                say("mirror", "\(kv[0]) = \(kv[1])")
            }
            let t = TerminalCollector()
            var terminalId = ""
            t.answer = { data in try? core.terminalInput(terminalId: terminalId, data: data) }
            terminalId = try await core.openTerminal(rows: 40, cols: 200, sink: t)
            func until(_ needle: String, _ limit: Double) async -> Bool {
                let deadline = Date().addingTimeInterval(limit)
                while Date() < deadline {
                    if t.text.contains(needle) { return true }
                    try? await Task.sleep(for: .milliseconds(100))
                }
                return false
            }
            _ = await until("root@solos:~# ", 30)
            let started = Date()
            let mark = "__probe_done_"
            try core.terminalInput(terminalId: terminalId, data: Data("\(command); echo \(mark)$?\n".utf8))
            // The echoed command line holds the mark too; the answer is the
            // mark at the start of a line, followed by the status.
            let finished = await until("\r\n\(mark)", seconds)
            let elapsed = Int(Date().timeIntervalSince(started) * 1000)
            // The end only: a progress bar can fill the console pipe, and a
            // blocked write never returns.
            say("shell_output", String(t.text.suffix(4000)).debugDescription)
            say("shell", finished ? "finished in \(elapsed)ms" : "FAIL still running after \(elapsed)ms")
            exit(finished ? 0 : 1)
        } catch {
            say("shell", "FAIL \(error)")
            exit(1)
        }
    }

    /// `SOLOS_PROBE_TERMINAL=1`: the person's terminal, without a screen.
    /// Checks the prompt and where it starts, that a large paste arrives
    /// whole, that a resize reaches the program, and that the end is
    /// reported. Prints `PROBE terminal_<check>: ok|FAIL …`; exits 0 when
    /// every check passed. Output lines end in "\r\n", which Swift treats as
    /// one character: search for "\r\nX", never "\nX".
    static func terminal() async {
        let env = ProcessInfo.processInfo.environment
        let dataDir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("SolosProbe")
        try? FileManager.default.createDirectory(at: dataDir, withIntermediateDirectories: true)
        var failed = false
        func check(_ name: String, _ ok: Bool, _ detail: String) {
            say("terminal_\(name)", ok ? "ok \(detail)" : "FAIL \(detail)")
            if !ok { failed = true }
        }
        do {
            let core = try await openCore(
                config: CoreConfig(dataDir: dataDir.path,
                                   bundledRootfs: Bundle.main.path(forResource: "alpine-rootfs", ofType: "zip"),
                                   captureDir: nil),
                secrets: EnvSecret(),
                browser: BrowserTools(),
                device: DeviceTools())
            try await core.bootSandbox()
            let t = TerminalCollector()
            let started = Date()
            var terminalId = ""
            if env["SOLOS_PROBE_ANSWER_CPR"] != "0" {
                t.answer = { data in try? core.terminalInput(terminalId: terminalId, data: data) }
            }
            let id = try await core.openTerminal(rows: 30, cols: 70, sink: t)
            terminalId = id
            func until(_ needle: String, _ seconds: Double = 20) async -> Bool {
                let deadline = Date().addingTimeInterval(seconds)
                while Date() < deadline {
                    if t.text.contains(needle) { return true }
                    try? await Task.sleep(for: .milliseconds(50))
                }
                return false
            }
            let prompt = await until("root@solos:~# ")
            check("prompt", prompt, "\(Int(Date().timeIntervalSince(started) * 1000))ms \(t.text.suffix(40).debugDescription)")

            try core.terminalInput(terminalId: id, data: Data("echo where-$(pwd)-$((6*7)); stty size\n".utf8))
            let cwd = await until("where-/root-42")
            let size = await until("30 70")
            check("cwd_and_size", cwd && size, t.text.suffix(80).debugDescription)

            // Ctrl-C interrupts the program in front.
            try core.terminalInput(terminalId: id, data: Data("cat > /tmp/c\n".utf8))
            try await Task.sleep(for: .milliseconds(500))
            try core.terminalInput(terminalId: id, data: Data("abc\n".utf8))
            try await Task.sleep(for: .milliseconds(300))
            try core.terminalInput(terminalId: id, data: Data([0x03]))
            try core.terminalInput(terminalId: id, data: Data("echo intr-$(wc -c < /tmp/c)\n".utf8))
            check("ctrl_c", await until("intr-4", 10), t.text.suffix(80).debugDescription)

            // Ctrl-D ends `cat`, sent apart and sent with the text.
            try core.terminalInput(terminalId: id, data: Data("cat > /tmp/a\n".utf8))
            try await Task.sleep(for: .milliseconds(300))
            try core.terminalInput(terminalId: id, data: Data("abc\n".utf8))
            try await Task.sleep(for: .milliseconds(300))
            try core.terminalInput(terminalId: id, data: Data([0x04]))
            try core.terminalInput(terminalId: id, data: Data("echo eof-apart-$(wc -c < /tmp/a)\n".utf8))
            check("ctrl_d_apart", await until("eof-apart-4", 10), t.text.suffix(80).debugDescription)
            try core.terminalInput(terminalId: id, data: Data("cat > /tmp/b\n".utf8))
            try await Task.sleep(for: .milliseconds(300))
            try core.terminalInput(terminalId: id, data: Data("abcd\n".utf8) + Data([0x04]))
            try core.terminalInput(terminalId: id, data: Data("echo eof-together-$(wc -c < /tmp/b)\n".utf8))
            check("ctrl_d_together", await until("eof-together-5", 10), t.text.suffix(80).debugDescription)

            // Through the kernel's line discipline alone: `cat` reads the
            // lines, the shell's line editor is not involved.
            for lines in [20, 40, 80, 160, 400] {
                try core.terminalInput(terminalId: id, data: Data("cat > /tmp/p\(lines).txt\n".utf8))
                try await Task.sleep(for: .milliseconds(300))
                var body = ""
                for _ in 0..<lines { body += String(repeating: "x", count: 99) + "\n" }
                let started = Date()
                try core.terminalInput(terminalId: id, data: Data(body.utf8) + Data([0x04]))
                try core.terminalInput(terminalId: id, data: Data("echo got-\(lines)-$(wc -c < /tmp/p\(lines).txt)\n".utf8))
                let ok = await until("got-\(lines)-\(lines * 100)", 20)
                check("paste_\(lines * 100)_to_cat", ok, "\(Int(Date().timeIntervalSince(started) * 1000))ms")
                if !ok { break }
            }

            // Into the shell itself, as a pasted command block is.
            var lines = "wc -c <<'EOF'\n"
            for _ in 0..<20 { lines += String(repeating: "y", count: 60) + "\n" }
            lines += "EOF\n"
            let shellStarted = Date()
            try core.terminalInput(terminalId: id, data: Data(lines.utf8))
            let shelled = await until("\r\n1220\r\n", 30)
            check("paste_20_lines_to_shell", shelled, "\(Int(Date().timeIntervalSince(shellStarted) * 1000))ms ending \(t.text.suffix(400).debugDescription)")

            try core.terminalResize(terminalId: id, rows: 41, cols: 99)
            try await Task.sleep(for: .milliseconds(300))
            try core.terminalInput(terminalId: id, data: Data("stty size\n".utf8))
            check("resize", await until("41 99"), t.text.suffix(40).debugDescription)

            let outStarted = Date()
            try core.terminalInput(terminalId: id, data: Data("seq 1 20000; echo seq-done\n".utf8))
            check("output_20000_lines", await until("20000\r\nseq-done", 120), "\(Int(Date().timeIntervalSince(outStarted) * 1000))ms")

            try core.terminalInput(terminalId: id, data: Data("exit 3\n".utf8))
            let deadline = Date().addingTimeInterval(10)
            while !t.ended && Date() < deadline { try? await Task.sleep(for: .milliseconds(50)) }
            check("exit", t.exitCode == .some(3), "\(String(describing: t.exitCode))")
            exit(failed ? 1 : 0)
        } catch {
            say("error", String(describing: error))
            exit(1)
        }
    }
}
