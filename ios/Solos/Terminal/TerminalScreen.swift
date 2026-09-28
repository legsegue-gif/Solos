import SwiftTerm
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// A shell of the person's own: the same Linux and the same files the
/// model works on (`cd /solos/ws`).
///
/// The emulator is SwiftTerm (xterm-compatible: answers the terminal's
/// queries, wide characters, 256 and true colour, bracketed paste, a
/// hardware keyboard, link detection, and drawing only what is visible);
/// this file connects it to the core's terminal and to the screen.
struct TerminalScreen: View {
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var session = TerminalSession()

    var body: some View {
        NavigationStack {
            // Not ignoring the keyboard: the terminal gets shorter when it
            // comes up, and the shell is told the rows it really has.
            TerminalHost(session: session)
                .background(Color.black.ignoresSafeArea(edges: .bottom))
                .navigationTitle(session.title ?? String(localized: "Terminal"))
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button(String(localized: "Done")) { dismiss() }
                    }
                    ToolbarItem(placement: .primaryAction) {
                        Menu {
                            Button { session.clearScreen() } label: {
                                Label(String(localized: "Clear screen"), systemImage: "eraser")
                            }
                            Button { session.restart() } label: {
                                Label(String(localized: "New shell"), systemImage: "arrow.clockwise")
                            }
                        } label: {
                            Label(String(localized: "More"), systemImage: "ellipsis")
                        }
                    }
                }
        }
        .onAppear { session.core = app.core }
        .onDisappear { session.close() }
        .onReceive(NotificationCenter.default.publisher(for: UIApplication.didBecomeActiveNotification)) { _ in
            // Coming back from another app leaves the keyboard down.
            session.focus()
        }
    }
}

private struct TerminalHost: UIViewRepresentable {
    let session: TerminalSession

    func makeUIView(context: Context) -> SwiftTerm.TerminalView {
        let view = PastableTerminalView(frame: .zero, font: UIFont.monospacedSystemFont(ofSize: 12, weight: .regular))
        view.onPaste = { [weak session] text in session?.paste(text) }
        // Light text on black, as the reference app's terminal; SwiftTerm's
        // own default grey read dim beside it.
        view.nativeBackgroundColor = .black
        view.nativeForegroundColor = UIColor(white: 0.9, alpha: 1)
        view.terminalDelegate = session
        view.inputAccessoryView = TerminalKeys(session: session, pasteTarget: view)
        // Tapping the screen brings the keyboard back after it was put away;
        // SwiftTerm's own taps (selection, links) still happen.
        let tap = UITapGestureRecognizer(target: session, action: #selector(TerminalSession.tapped))
        tap.cancelsTouchesInView = false
        view.addGestureRecognizer(tap)
        session.view = view
        DispatchQueue.main.async { session.focus() }
        return view
    }

    func updateUIView(_ view: SwiftTerm.TerminalView, context: Context) {}
}

/// One terminal: the view, the shell behind it, and the traffic between.
@MainActor
@Observable
final class TerminalSession: NSObject {
    @ObservationIgnored weak var view: SwiftTerm.TerminalView?
    @ObservationIgnored var core: SolosCore?
    private(set) var title: String?

    @ObservationIgnored private var terminalId: String?
    @ObservationIgnored private var opening = false
    /// Keystrokes typed while the shell was still starting.
    @ObservationIgnored private var early = Data()
    @ObservationIgnored private var resize: Task<Void, Never>?
    @ObservationIgnored private let output = OutputBuffer()

    func focus() {
        _ = view?.becomeFirstResponder()
    }

    @objc func tapped() {
        if view?.isFirstResponder == false { focus() }
    }

    /// Open once the view knows how many rows and columns it has: a shell
    /// opened at a guessed size wraps its first lines there for good.
    private func open(cols: Int, rows: Int) {
        guard terminalId == nil, !opening, let core, cols > 1, rows > 1 else { return }
        opening = true
        let sink = Sink(buffer: output) { [weak self] in self?.drain() } exited: { [weak self] code in
            Task { @MainActor in self?.ended(code) }
        }
        Task {
            defer { opening = false }
            do {
                let id = try await core.openTerminal(rows: UInt16(rows), cols: UInt16(cols), sink: sink)
                terminalId = id
                if !early.isEmpty {
                    try? core.terminalInput(terminalId: id, data: early)
                    early = Data()
                }
            } catch {
                let why = (error as? CoreError)?.message ?? error.localizedDescription
                view?.feed(text: "\r\n" + String(localized: "The terminal could not start: \(why)") + "\r\n")
            }
        }
    }

    /// Everything the shell printed since the last call, in one feed.
    private func drain() {
        let bytes = output.take()
        guard !bytes.isEmpty else { return }
        view?.feed(byteArray: ArraySlice(bytes))
    }

    private func ended(_ code: Int32?) {
        drain()
        terminalId = nil
        let note = code.map { String(localized: "[the shell ended with status \(String($0))]") } ?? String(localized: "[the shell ended]")
        view?.feed(text: "\r\n\(note)\r\n")
    }

    /// A key from the key row, as the terminal would send it.
    func key(_ bytes: [UInt8]) {
        send(Data(bytes))
    }

    func arrow(_ direction: Character) {
        let app = view?.getTerminal().applicationCursor ?? false
        let seq: [UInt8] = switch direction {
        case "A": app ? EscapeSequences.moveUpApp : EscapeSequences.moveUpNormal
        case "B": app ? EscapeSequences.moveDownApp : EscapeSequences.moveDownNormal
        case "C": app ? EscapeSequences.moveRightApp : EscapeSequences.moveRightNormal
        default: app ? EscapeSequences.moveLeftApp : EscapeSequences.moveLeftNormal
        }
        send(Data(seq))
    }

    func toggleControl() {
        view?.controlModifier.toggle()
    }

    var controlHeld: Bool { view?.controlModifier ?? false }

    func toggleKeyboard() {
        guard let view else { return }
        if view.isFirstResponder { _ = view.resignFirstResponder() } else { _ = view.becomeFirstResponder() }
    }

    /// Pasted text. Fenced when the program asked for it, so a pasted
    /// block is not run line by line as it arrives.
    func paste(_ text: String) {
        guard !text.isEmpty, let view else { return }
        let body = text.replacingOccurrences(of: "\r\n", with: "\r").replacingOccurrences(of: "\n", with: "\r")
        if view.getTerminal().bracketedPasteMode {
            send(Data("\u{1B}[200~\(body)\u{1B}[201~".utf8))
        } else {
            send(Data(body.utf8))
        }
    }

    /// The screen and the lines scrolled off it.
    func clearScreen() {
        view?.feed(text: "\u{1B}[H\u{1B}[2J\u{1B}[3J")
        send(Data([0x0C]))
    }

    func restart() {
        if let terminalId { try? core?.terminalClose(terminalId: terminalId) }
        terminalId = nil
        view?.getTerminal().resetToInitialState()
        view?.feed(text: "\u{1B}[H\u{1B}[2J\u{1B}[3J")
        if let t = view?.getTerminal() { open(cols: t.cols, rows: t.rows) }
    }

    func close() {
        resize?.cancel()
        if let terminalId { try? core?.terminalClose(terminalId: terminalId) }
        terminalId = nil
    }

    fileprivate func send(_ data: Data) {
        guard let terminalId, let core else {
            early.append(data)
            return
        }
        try? core.terminalInput(terminalId: terminalId, data: data)
    }
}

extension TerminalSession: TerminalViewDelegate {
    nonisolated func send(source: SwiftTerm.TerminalView, data: ArraySlice<UInt8>) {
        let bytes = Data(data)
        MainActor.assumeIsolated {
            send(bytes)
            // A held Ctrl applies to one key; show it released.
            (source.inputAccessoryView as? TerminalKeys)?.refresh()
        }
    }

    nonisolated func sizeChanged(source: SwiftTerm.TerminalView, newCols: Int, newRows: Int) {
        MainActor.assumeIsolated {
            guard let terminalId, let core else {
                open(cols: newCols, rows: newRows)
                return
            }
            // The keyboard's animation relays out many times in a quarter of
            // a second; the shell hears only the size it settles on.
            resize?.cancel()
            resize = Task {
                try? await Task.sleep(for: .milliseconds(400))
                guard !Task.isCancelled else { return }
                try? core.terminalResize(terminalId: terminalId, rows: UInt16(newRows), cols: UInt16(newCols))
            }
        }
    }

    nonisolated func setTerminalTitle(source: SwiftTerm.TerminalView, title: String) {
        MainActor.assumeIsolated { self.title = title.isEmpty ? nil : title }
    }

    nonisolated func requestOpenLink(source: SwiftTerm.TerminalView, link: String, params: [String: String]) {
        guard let url = URL(string: link), ["http", "https"].contains(url.scheme?.lowercased()) else { return }
        MainActor.assumeIsolated { UIApplication.shared.open(url) }
    }

    nonisolated func clipboardCopy(source: SwiftTerm.TerminalView, content: Data) {
        if let text = String(data: content, encoding: .utf8) {
            MainActor.assumeIsolated { UIPasteboard.general.string = text }
        }
    }

    nonisolated func hostCurrentDirectoryUpdate(source: SwiftTerm.TerminalView, directory: String?) {}
    nonisolated func scrolled(source: SwiftTerm.TerminalView, position: Double) {}
    nonisolated func bell(source: SwiftTerm.TerminalView) {}
    nonisolated func iTermContent(source: SwiftTerm.TerminalView, content: ArraySlice<UInt8>) {}
    nonisolated func rangeChanged(source: SwiftTerm.TerminalView, startY: Int, endY: Int) {}
}

/// Output waiting for the main thread. The core hands it over on its own
/// threads; the main thread is woken once per batch, not once per chunk.
private final class OutputBuffer: @unchecked Sendable {
    private let lock = NSLock()
    private var pending = Data()

    /// Returns true when this made the buffer non-empty: the caller then
    /// wakes the main thread.
    func append(_ data: Data) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let wasEmpty = pending.isEmpty
        pending.append(data)
        return wasEmpty
    }

    func take() -> [UInt8] {
        lock.lock()
        defer { lock.unlock() }
        let out = [UInt8](pending)
        pending = Data()
        return out
    }
}

private final class Sink: TerminalSink, @unchecked Sendable {
    let buffer: OutputBuffer
    let wake: @MainActor () -> Void
    let onExit: @Sendable (Int32?) -> Void

    init(buffer: OutputBuffer, wake: @escaping @MainActor () -> Void, exited: @escaping @Sendable (Int32?) -> Void) {
        self.buffer = buffer
        self.wake = wake
        self.onExit = exited
    }

    func output(data: Data) {
        if buffer.append(data) {
            let wake = self.wake
            DispatchQueue.main.async { MainActor.assumeIsolated { wake() } }
        }
    }

    func exited(code: Int32?) {
        // After the last output has been handed over.
        let onExit = self.onExit
        DispatchQueue.main.async { onExit(code) }
    }
}

/// The keys a phone keyboard lacks, above it: the reference app's row
/// (keyboard, paste, esc, tab, return, ctrl, arrows) and the symbols that
/// take three taps on a phone keyboard. Return is a key of its own because
/// the system keyboard's return does not always send a carriage return.
final class TerminalKeys: UIInputView {
    private weak var session: TerminalSession?
    private var ctrlButton: UIButton?

    init(session: TerminalSession, pasteTarget: UIResponder & UIPasteConfigurationSupporting) {
        self.session = session
        super.init(frame: CGRect(x: 0, y: 0, width: 0, height: 44), inputViewStyle: .keyboard)
        allowsSelfSizing = true
        // The terminal's keyboard is dark; the keys above it follow, or
        // their labels are drawn dark on dark.
        overrideUserInterfaceStyle = .dark
        let scroll = UIScrollView()
        scroll.showsHorizontalScrollIndicator = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        let row = UIStackView()
        row.axis = .horizontal
        row.spacing = 6
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(scroll)
        scroll.addSubview(row)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.topAnchor.constraint(equalTo: topAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            heightAnchor.constraint(equalToConstant: 44),
            row.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 8),
            row.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -8),
            row.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
            row.heightAnchor.constraint(equalToConstant: 34),
        ])
        let keys: [(String, String?, String, () -> Void)] = [
            ("", "keyboard.chevron.compact.down", String(localized: "Keyboard"), { [weak session] in session?.toggleKeyboard() }),
            ("esc", nil, "Escape", { [weak session] in session?.key(EscapeSequences.cmdEsc) }),
            ("tab", nil, "Tab", { [weak session] in session?.key(EscapeSequences.cmdTab) }),
            ("", "return", String(localized: "Return"), { [weak session] in session?.key(EscapeSequences.cmdRet) }),
            ("ctrl", nil, "Control", { [weak self, weak session] in session?.toggleControl(); self?.refresh() }),
            ("", "arrow.left", String(localized: "Left"), { [weak session] in session?.arrow("D") }),
            ("", "arrow.down", String(localized: "Down"), { [weak session] in session?.arrow("B") }),
            ("", "arrow.up", String(localized: "Up"), { [weak session] in session?.arrow("A") }),
            ("", "arrow.right", String(localized: "Right"), { [weak session] in session?.arrow("C") }),
            ("~", nil, "~", { [weak session] in session?.key(Array("~".utf8)) }),
            ("|", nil, "|", { [weak session] in session?.key(Array("|".utf8)) }),
            ("/", nil, "/", { [weak session] in session?.key(Array("/".utf8)) }),
            ("-", nil, "-", { [weak session] in session?.key(Array("-".utf8)) }),
        ]
        for (index, (title, symbol, label, action)) in keys.enumerated() {
            if index == 1 {
                // The system's own paste button: it reads the clipboard
                // without the "Allow Paste" prompt a plain read would raise.
                let config = UIPasteControl.Configuration()
                config.displayMode = .labelOnly
                config.cornerStyle = .medium
                let paste = UIPasteControl(configuration: config)
                paste.target = pasteTarget
                row.addArrangedSubview(paste)
            }
            var config = UIButton.Configuration.gray()
            config.cornerStyle = .medium
            config.contentInsets = NSDirectionalEdgeInsets(top: 4, leading: 10, bottom: 4, trailing: 10)
            if let symbol { config.image = UIImage(systemName: symbol) } else { config.title = title }
            let button = UIButton(configuration: config, primaryAction: UIAction { _ in
                UIDevice.current.playInputClick()
                action()
            })
            button.accessibilityLabel = label
            if label == "Control" { ctrlButton = button }
            row.addArrangedSubview(button)
        }
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    /// Show whether Ctrl is waiting for its key.
    func refresh() {
        guard let ctrlButton else { return }
        var config = ctrlButton.configuration
        let held = session?.controlHeld ?? false
        config?.baseBackgroundColor = held ? .tintColor : nil
        config?.baseForegroundColor = held ? .white : nil
        ctrlButton.configuration = config
    }
}

extension TerminalKeys: UIInputViewAudioFeedback {
    var enableInputClicksWhenVisible: Bool { true }
}

/// SwiftTerm's view, taking text from the system paste button.
final class PastableTerminalView: SwiftTerm.TerminalView {
    var onPaste: ((String) -> Void)?

    override init(frame: CGRect, font: UIFont?) {
        super.init(frame: frame, font: font)
        pasteConfiguration = UIPasteConfiguration(acceptableTypeIdentifiers: [UTType.plainText.identifier, UTType.text.identifier])
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override func paste(itemProviders: [NSItemProvider]) {
        guard let provider = itemProviders.first(where: { $0.canLoadObject(ofClass: NSString.self) }) else { return }
        _ = provider.loadObject(ofClass: NSString.self) { [weak self] object, _ in
            guard let text = object as? String else { return }
            DispatchQueue.main.async { self?.onPaste?(text) }
        }
    }
}
