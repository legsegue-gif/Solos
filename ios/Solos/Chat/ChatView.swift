import GameController
import PhotosUI
import SwiftUI

struct ChatView: View {
    let sessionId: String?
    @Environment(AppCore.self) private var app
    @State private var model: ChatModel?
    @State private var draft = ""
    @State private var pickingModel = false
    /// The user message being edited; sending replaces it and answers again.
    @State private var editing: Message?
    @State private var confirmingClear = false
    @State private var showingTerminal = false
    @State private var showingFiles = false
    @State private var showingBrowser = false
    @State private var attachments: [PendingAttachment] = []
    @State private var pickingPhotos = false
    @State private var photoItems: [PhotosPickerItem] = []
    @State private var pickingFiles = false
    @State private var attachError: String?
    /// Following the reply as it streams. Only the reader's own hand turns
    /// it off, by scrolling away; "is the view at the bottom" cannot be the
    /// gate, because growth arrives a frame before the scroll that follows
    /// it and the gate would close itself.
    @State private var following = true
    /// Moved by the reader, and how far from the end it was on the last
    /// frame. Not drawn, so not view state.
    @State private var drive = ScrollDrive()
    @State private var atTop = false
    /// Drives "go to the end": the content's own bottom edge, not a marker
    /// near it. Scrolling to a marker left long chats blank.
    @State private var scrollPosition = ScrollPosition()
    /// Two points count as the end; while a reply streams, a line (20pt)
    /// behind still counts as with it.
    private static let endSlack: CGFloat = 2
    private static let liveEndSlack: CGFloat = 20
    @FocusState private var composerFocused: Bool
    /// Bumped on every send; the text field is keyed on it, so a send builds
    /// a fresh field. Backstop for a hardware keyboard, where focus stays and
    /// the old input session could write the sent text back.
    @State private var composerGeneration = 0

    var body: some View {
        Group {
            if let model {
                content(model)
            } else {
                ProgressView()
            }
        }
        .navigationTitle(model?.state.session?.title ?? String(localized: "New chat"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) { header }
            ToolbarItem(placement: .topBarTrailing) {
                // The reference app's order and grouping.
                Menu {
                    Button { app.newChatRequested = true } label: {
                        Label(String(localized: "New chat"), systemImage: "square.and.pencil")
                    }
                    Divider()
                    Button { Task { await model?.compact(thenContinue: false) } } label: {
                        Label(String(localized: "Summarize conversation"), systemImage: "text.redaction")
                    }
                    .disabled(model?.state.session == nil || model?.state.turnRunning == true || model?.compacting == true)
                    Button(role: .destructive) { confirmingClear = true } label: {
                        Label(String(localized: "Clear messages"), systemImage: "trash")
                    }
                    .disabled(model?.state.messages.isEmpty != false || model?.state.turnRunning == true)
                    Divider()
                    Button { showingFiles = true } label: {
                        Label(String(localized: "Browse files"), systemImage: "folder")
                    }
                    Button { showingTerminal = true } label: {
                        Label(String(localized: "Open terminal"), systemImage: "apple.terminal")
                    }
                    Button { showingBrowser = true } label: {
                        Label(String(localized: "Open browser"), systemImage: "globe")
                    }
                    Divider()
                    Button { Task { await model?.copySessionData() } } label: {
                        Label(String(localized: "Copy session data"), systemImage: "doc.on.clipboard")
                    }
                    .disabled(model?.state.session == nil)
                } label: {
                    Label(String(localized: "More"), systemImage: "ellipsis")
                }
            }
        }
        .confirmationDialog(String(localized: "Delete every message in this chat?"), isPresented: $confirmingClear, titleVisibility: .visible) {
            Button(String(localized: "Clear messages"), role: .destructive) { Task { await model?.clear() } }
        }
        .fullScreenCover(isPresented: $showingTerminal) { TerminalScreen() }
        .sheet(isPresented: $showingFiles) { FileBrowser() }
        .sheet(isPresented: $showingBrowser) { NavigationStack { BrowserView() } }
        .sheet(isPresented: $pickingModel) {
            if let session = model?.state.session { ModelPicker(session: session) }
        }
        .task {
            guard model == nil else { return }
            let m = ChatModel(app: app)
            model = m
            await m.open(sessionId)
        }
    }

    /// Title, and under it the model with the thinking state: tapping it
    /// changes either.
    private var header: some View {
        let session = model?.state.session
        let thinking = session?.thinking ?? app.settings.thinking
        return VStack(spacing: 1) {
            Text(session?.title ?? String(localized: "New chat"))
                .font(.headline).lineLimit(1)
            Button { pickingModel = true } label: {
                HStack(spacing: 4) {
                    Text(modelLine(session?.model))
                    Text(thinking ? String(localized: "Thinking on") : String(localized: "Thinking off"))
                        .padding(.horizontal, 5).padding(.vertical, 1)
                        .background(Color(.tertiarySystemFill), in: Capsule())
                    Image(systemName: "chevron.down").imageScale(.small)
                }
                .font(.caption).foregroundStyle(.secondary)
            }
            .disabled(session == nil)
            .accessibilityIdentifier("modelButton")
        }
    }

    /// "Endpoint · model", as the reference app writes it: relays often
    /// serve the same model id, and the answer depends on which one.
    private func modelLine(_ choice: ModelChoice?) -> String {
        guard let choice else { return String(localized: "No model") }
        guard let ep = app.settings.endpoints.first(where: { $0.id == choice.endpointId }) else { return choice.model }
        return "\(ep.name) · \(choice.model)"
    }

    @ViewBuilder
    private func content(_ model: ChatModel) -> some View {
        let state = model.state
        let showThinking = state.session?.thinking ?? app.settings.thinking
        let turns: [String] = state.messages.filter { $0.role == .user }.map(\.id)
        ScrollViewReader { proxy in
            ScrollView {
                // Not lazy: with replies tens of thousands of characters
                // long, a lazy stack's estimated heights put the view on
                // rows that were never laid out, and the chat showed blank
                // (measured).
                VStack(alignment: .leading, spacing: 14) {
                    let ends = state.answerEnds
                    ForEach(state.messages, id: \.id) { m in
                        if m.role != .tool {
                            MessageRow(message: m, state: state, showThinking: showThinking,
                                       onEdit: { startEditing(m) },
                                       onRetry: { Task { await model.retry(from: m.id) } },
                                       answer: ends[m.id].map { end in
                                           AnswerActions(text: end.text) { Task { await model.retry(from: end.question) } }
                                       })
                                .id(m.id)
                                .modifier(QuestionTop(isQuestion: m.role == .user, id: m.id, drive: drive))
                        }
                        if let c = state.compaction(after: m.id), !c.undone {
                            CompactionDivider(compaction: c) { Task { await model.undoCompaction(c.id) } }
                        }
                    }
                    if let streaming = state.streaming {
                        MessageRow(message: streaming, state: state, showThinking: showThinking, onEdit: nil, onRetry: nil, answer: nil)
                    }
                    if state.turnRunning && state.streaming == nil {
                        ProgressView().padding(.vertical, 4)
                    }
                    if let retrying = state.retrying {
                        Text("Retrying: \(retrying.message)").font(.caption).foregroundStyle(.secondary)
                    }
                    if let error = model.sendError ?? state.lastError {
                        Text(error.message).font(.callout).foregroundStyle(.red)
                    }
                    if model.compacting {
                        HStack(spacing: 8) {
                            ProgressView()
                            Text("Summarizing the earlier conversation…").font(.callout).foregroundStyle(.secondary)
                        }
                    } else if !state.turnRunning, let room = state.lastError?.roomAction {
                        HStack(spacing: 10) {
                            if room == .summarize {
                                Button { Task { await model.compact(thenContinue: true) } } label: {
                                    Label(String(localized: "Summarize and continue"), systemImage: "text.redaction")
                                }
                            }
                            Button { app.newChatRequested = true } label: {
                                Label(String(localized: "New chat"), systemImage: "square.and.pencil")
                            }
                        }
                        .buttonStyle(.bordered).font(.callout)
                    } else if !state.turnRunning && (state.canResume || state.lastError != nil) {
                        HStack(spacing: 10) {
                            if state.canResume {
                                Button { Task { await model.resume() } } label: {
                                    Label(String(localized: "Continue"), systemImage: "play.fill")
                                }
                            }
                            Button { Task { _ = await model.retry(from: nil) } } label: {
                                Label(String(localized: "Retry"), systemImage: "arrow.counterclockwise")
                            }
                        }
                        .buttonStyle(.bordered).font(.callout)
                    }
                    ForEach(state.queued, id: \.self) { q in
                        Text(q).font(.callout).foregroundStyle(.secondary)
                            .padding(10).frame(maxWidth: .infinity, alignment: .trailing)
                    }
                }
                .scrollTargetLayout()
                .padding(.horizontal)
                .padding(.vertical, 12)
                .coordinateSpace(name: Self.contentSpace)
            }
            .scrollPosition($scrollPosition)
            .scrollDismissesKeyboard(.interactively)
            .defaultScrollAnchor(.bottom)
            // Where the ends are: a pure read, called back only when an
            // answer flips. The top inset shifts the coordinate space, so it
            // is subtracted.
            .onScrollGeometryChange(for: ScrollEnds.self) { g in
                let gap = g.contentSize.height - g.containerSize.height - g.contentInsets.top - g.contentOffset.y
                return ScrollEnds(atTop: g.contentOffset.y <= -g.contentInsets.top + Self.endSlack,
                                  atBottom: gap <= Self.endSlack)
            } action: { _, ends in
                atTop = ends.atTop
            }
            // Where the visible part starts, in content coordinates: the
            // step buttons compare question rows against it.
            .onScrollGeometryChange(for: CGFloat.self) { g in
                g.contentOffset.y + g.contentInsets.top
            } action: { _, top in
                drive.visibleTop = top
            }
            // Every frame, for gestures: the flip-only reader above can miss
            // the pull away from a streaming end.
            .onScrollGeometryChange(for: CGFloat.self) { g in
                g.contentSize.height - g.containerSize.height - g.contentInsets.top - g.contentOffset.y
            } action: { _, gap in
                drive.gap = gap
                // A step that ends at the bottom is the same as "go to the
                // end": when the last answer is short, its question cannot
                // reach the top, and the lower buttons stayed up at the end.
                if !drive.byUser, !following, gap <= Self.endSlack {
                    following = true
                    return
                }
                guard drive.byUser, gap > Self.liveEndSlack, following else { return }
                following = false
            }
            .onScrollPhaseChange { _, phase in
                switch phase {
                case .interacting:
                    drive.byUser = true
                case .idle:
                    guard drive.byUser else { break }
                    following = drive.gap <= Self.liveEndSlack
                    drive.byUser = false
                default:
                    break
                }
            }
            // Every applied event; unanimated, since this runs per delta.
            .onChange(of: state.seq) { if following { scrollPosition.scrollTo(edge: .bottom) } }
            .onChange(of: state.session?.id) {
                following = true
                scrollPosition.scrollTo(edge: .bottom)
            }
            .overlay(alignment: .topTrailing) { backwardButtons(proxy, turns: turns, state: state) }
            .overlay(alignment: .bottomTrailing) { forwardButtons(proxy, turns: turns, state: state) }
            .modifier(FileLinks())
        }
        .safeAreaInset(edge: .bottom) { composer(model) }
    }

    // MARK: - Jumping around

    fileprivate static let contentSpace = "transcript"
    /// A question this close to the top counts as the one at the top; more
    /// than the list's 12 pt top padding, so "next" from the very top goes
    /// past the first question rather than 12 pt down to it.
    private static let stepSlack: CGFloat = 16

    /// Back to the beginning and to the previous question: offered whenever
    /// there is somewhere to go back to. The step button only with more
    /// than one question (with one, it would be the top button again).
    @ViewBuilder
    private func backwardButtons(_ proxy: ScrollViewProxy, turns: [String], state: ChatState) -> some View {
        if !state.messages.isEmpty && !atTop {
            VStack(spacing: 8) {
                Button { jumpToTop() } label: { floatingGlyph("arrow.up.to.line") }
                    .accessibilityLabel(String(localized: "Go to the beginning"))
                if turns.count > 1 {
                    Button { walkUp(proxy, turns: turns) } label: { floatingGlyph("chevron.up") }
                        .accessibilityLabel(String(localized: "Previous question"))
                }
            }
            .padding(.trailing, 12).padding(.top, 12)
        }
    }

    /// The next question and the end: only once the reader has left the
    /// stream, since "go to the end" has nothing to say while following it.
    @ViewBuilder
    private func forwardButtons(_ proxy: ScrollViewProxy, turns: [String], state: ChatState) -> some View {
        if !following && !state.messages.isEmpty {
            VStack(spacing: 8) {
                if turns.count > 1 {
                    Button { walkDown(proxy, turns: turns) } label: { floatingGlyph("chevron.down") }
                        .accessibilityLabel(String(localized: "Next question"))
                }
                Button { jumpToBottom() } label: { floatingGlyph("arrow.down.to.line") }
                    .accessibilityLabel(String(localized: "Go to the end"))
            }
            .padding(.trailing, 12).padding(.bottom, 12)
        }
    }

    private func floatingGlyph(_ name: String) -> some View {
        Image(systemName: name)
            .font(.system(size: 14, weight: .semibold))
            .foregroundStyle(.secondary)
            .frame(width: 36, height: 36)
            .background(Color(.systemBackground).opacity(0.94), in: Circle())
            .overlay(Circle().stroke(Color.gray.opacity(0.35), lineWidth: 0.5))
            .shadow(color: .black.opacity(0.18), radius: 5, y: 2)
    }

    /// To the content's own top edge, as "go to the end" goes to its bottom
    /// edge. A marker row sat inside the list's padding, 12 pt short of the
    /// edge, so the top buttons stayed up after it until a nudge.
    private func jumpToTop() {
        following = false
        withAnimation(.easeOut(duration: 0.2)) { scrollPosition.scrollTo(edge: .top) }
    }

    private func jumpToBottom() {
        following = true
        withAnimation(.easeOut(duration: 0.2)) { scrollPosition.scrollTo(edge: .bottom) }
    }

    /// Stops following the stream: the reader asked to be somewhere else,
    /// and the next streamed token must not pull them back.
    private func jump(_ proxy: ScrollViewProxy, to id: String) {
        following = false
        withAnimation(.easeOut(duration: 0.2)) { proxy.scrollTo(id, anchor: .top) }
    }

    /// The question being read: the last one whose row starts at or above
    /// the top of what is on screen. Taken from the rows' own positions;
    /// the scroll position's view id stayed empty here (the view is scrolled
    /// by edge, not by row), and both step buttons went wrong with it.
    private func currentTurn(_ turns: [String]) -> Int? {
        turns.lastIndex { (drive.questionTops[$0] ?? .infinity) <= drive.visibleTop + Self.stepSlack }
    }

    /// Up: first to the start of the question being read, then past it.
    private func walkUp(_ proxy: ScrollViewProxy, turns: [String]) {
        guard let current = currentTurn(turns) else { return jumpToTop() }
        let atItsStart = (drive.questionTops[turns[current]] ?? 0) >= drive.visibleTop - Self.stepSlack
        let target = atItsStart ? current - 1 : current
        guard turns.indices.contains(target) else { return jumpToTop() }
        jump(proxy, to: turns[target])
    }

    /// Down: the next question below the top of the screen; past the last, the end.
    private func walkDown(_ proxy: ScrollViewProxy, turns: [String]) {
        let next = turns.firstIndex { (drive.questionTops[$0] ?? -.infinity) > drive.visibleTop + Self.stepSlack }
        guard let next else { return jumpToBottom() }
        jump(proxy, to: turns[next])
    }

    private func startEditing(_ m: Message) {
        editing = m
        draft = m.text
        composerFocused = true
    }

    private func composer(_ model: ChatModel) -> some View {
        VStack(spacing: 6) {
            if !attachments.isEmpty {
                PendingAttachmentsRow(items: attachments) { item in
                    item.discard()
                    attachments.removeAll { $0.id == item.id }
                }
            }
            if let attachError {
                Text(attachError).font(.caption).foregroundStyle(.red).frame(maxWidth: .infinity, alignment: .leading)
            }
            if let editing {
                HStack {
                    Label(String(localized: "Editing message"), systemImage: "pencil").font(.caption)
                    Spacer()
                    Button {
                        self.editing = nil
                        draft = ""
                    } label: { Image(systemName: "xmark.circle.fill") }
                    .accessibilityLabel(String(localized: "Cancel editing"))
                    .accessibilityIdentifier("cancelEditing \(editing.id)")
                }
                .foregroundStyle(.secondary)
            }
            composerRow(model)
        }
        .padding(.horizontal).padding(.vertical, 8)
        .background(.bar)
        .photosPicker(isPresented: $pickingPhotos, selection: $photoItems, maxSelectionCount: 10, matching: .images)
        .onChange(of: photoItems) { _, items in
            guard !items.isEmpty else { return }
            photoItems = []
            Task { await addPhotos(items) }
        }
        .fileImporter(isPresented: $pickingFiles, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            switch result {
            case .success(let urls):
                for url in urls {
                    do { attachments.append(try PendingAttachment.file(url)) } catch { attachError = error.localizedDescription }
                }
            case .failure(let error):
                attachError = error.localizedDescription
            }
        }
    }

    private func addPhotos(_ items: [PhotosPickerItem]) async {
        attachError = nil
        for (i, item) in items.enumerated() {
            do {
                if let a = try await PendingAttachment.photo(item, index: i) { attachments.append(a) }
            } catch {
                attachError = error.localizedDescription
            }
        }
    }

    /// With the soft keyboard, focus is dropped first: resigning commits what
    /// the input method still holds (a pinyin candidate, dictation, an
    /// autocorrection) and ends its session, so nothing writes the sent text
    /// back into the field, and the keyboard goes away while the reply
    /// streams. With a hardware keyboard focus stays, as in the reference app
    /// (`performSend`), so the next message needs no tap.
    private func send(_ model: ChatModel) {
        let softKeyboard = GCKeyboard.coalesced == nil
        if softKeyboard {
            composerFocused = false
            // SwiftUI's focus and UIKit's first responder can disagree; this
            // is what puts the keyboard away.
            UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
        }
        let text = draft
        let edited = editing
        let files = attachments
        // Sending is the one thing that always lands at the end.
        jumpToBottom()
        draft = ""
        editing = nil
        attachments = []
        composerGeneration += 1
        Task {
            // The rebuilt field does not inherit first responder.
            if !softKeyboard { composerFocused = true }
            let ok: Bool
            if let edited {
                ok = await model.retry(from: edited.id, text: text.trimmingCharacters(in: .whitespacesAndNewlines))
            } else {
                ok = await model.send(text, attachments: files)
            }
            if !ok { draft = text; editing = edited; attachments = files }
        }
    }

    private func composerRow(_ model: ChatModel) -> some View {
        HStack(alignment: .bottom, spacing: 8) {
            Menu {
                Button { pickingPhotos = true } label: { Label(String(localized: "Photos"), systemImage: "photo.on.rectangle") }
                Button { pickingFiles = true } label: { Label(String(localized: "Files"), systemImage: "folder") }
            } label: {
                Image(systemName: "plus.circle.fill").font(.title).foregroundStyle(.secondary)
            }
            .accessibilityLabel(String(localized: "Attach"))
            .disabled(editing != nil)
            TextField(String(localized: "Message"), text: $draft, axis: .vertical)
                .lineLimit(1...6)
                .focused($composerFocused)
                .id(composerGeneration)
                .padding(.horizontal, 12).padding(.vertical, 9)
                .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 18))
            if model.state.turnRunning && draft.isEmpty && attachments.isEmpty {
                Button { model.stop() } label: {
                    Image(systemName: "stop.circle.fill").font(.title)
                }
                .accessibilityLabel(String(localized: "Stop"))
            } else {
                Button { send(model) } label: {
                    Image(systemName: "arrow.up.circle.fill").font(.title)
                }
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && attachments.isEmpty)
                .accessibilityLabel(String(localized: "Send"))
            }
        }
    }
}

extension ChatState {
    /// The last message of each finished answer, with the whole answer's
    /// text (one answer can span several messages around its tool calls)
    /// and the question it answers. The answer being written has none.
    var answerEnds: [String: (text: String, question: String)] {
        var ends: [String: (text: String, question: String)] = [:]
        var question: String?
        var last: String?
        var texts: [String] = []
        func close() {
            if let question, let last, !texts.isEmpty { ends[last] = (texts.joined(separator: "\n\n"), question) }
        }
        for m in messages {
            switch m.role {
            case .user:
                close()
                question = m.id
                last = nil
                texts = []
            case .assistant:
                last = m.id
                if !m.text.isEmpty { texts.append(m.text) }
            case .tool:
                break
            }
        }
        if !turnRunning { close() }
        return ends
    }
}

private struct MessageRow: View {
    let message: Message
    let state: ChatState
    /// Thinking is off for this chat: its thinking is not drawn, as in the
    /// reference app (some endpoints think by model whatever is asked).
    let showThinking: Bool
    /// Nil while the message is still streaming or a turn runs.
    let onEdit: (() -> Void)?
    let onRetry: (() -> Void)?
    /// Set on the last message of a finished answer.
    let answer: AnswerActions?

    private var bubble: some View {
        Text(message.text)
            .padding(10)
            .background(Color.accentColor.opacity(0.15), in: RoundedRectangle(cornerRadius: 14))
            .contentShape(.contextMenuPreview, RoundedRectangle(cornerRadius: 14))
            .contextMenu {
                Button { UIPasteboard.general.string = message.text } label: {
                    Label(String(localized: "Copy"), systemImage: "doc.on.doc")
                }
                if !state.turnRunning, let onEdit {
                    Button(action: onEdit) { Label(String(localized: "Edit"), systemImage: "square.and.pencil") }
                }
                if !state.turnRunning, let onRetry {
                    Button(action: onRetry) { Label(String(localized: "Retry"), systemImage: "arrow.counterclockwise") }
                }
            }
    }

    var body: some View {
        switch message.role {
        case .user:
            VStack(alignment: .trailing, spacing: 6) {
                SentAttachments(parts: message.parts)
                if !message.text.isEmpty { bubble }
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
        case .assistant:
            VStack(alignment: .leading, spacing: 8) {
                ForEach(Array(message.parts.enumerated()), id: \.offset) { _, part in
                    if showThinking || !part.isThinking {
                        PartView(part: part, state: state)
                    }
                }
                // Finished replies only (the streaming one has no retry):
                // a long press on the text selects part of it, so the whole
                // reply needs a target that is easy to hit.
                if let answer { answer }
            }
            // On the background, as the reference app does: a long press on
            // the words selects them, one on the space around them copies
            // the whole reply.
            .background {
                Color.clear
                    .contentShape(Rectangle())
                    .contextMenu {
                        if !message.text.isEmpty {
                            Button { UIPasteboard.general.string = message.text } label: {
                                Label(String(localized: "Copy All"), systemImage: "doc.on.doc")
                            }
                        }
                    }
            }
        case .tool:
            EmptyView()
        }
    }
}

private struct PartView: View {
    let part: Part
    let state: ChatState
    @State private var showThinking = false
    @State private var detail: ToolDetail.Item?

    var body: some View {
        switch part {
        case .text(let text):
            MarkdownView(text: text)
        case .thinking(let text, _, _, _) where text.isEmpty:
            // Encrypted or undisplayed thinking: nothing to read.
            EmptyView()
        case .thinking(let text, _, _, _):
            DisclosureGroup(isExpanded: $showThinking) {
                Text(text).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            } label: {
                Label(String(localized: "Thinking"), systemImage: "brain").font(.caption).foregroundStyle(.secondary)
            }
        case .toolCall(let id, let name, let input, let title, _):
            ToolRow(callId: id, name: name, inputJson: input, title: title, state: state)
        case .toolResult, .attachment:
            // Results show in their call's row; attachments only come from
            // the user, whose row draws them.
            EmptyView()
        }
    }
}

/// One tool call: always a single row. Arguments and output are in the
/// detail sheet, so a long result never pushes the answer off the screen.
struct ToolRow: View {
    let callId: String
    let name: String
    let inputJson: String
    let title: String?
    let state: ChatState
    @State private var showing = false

    var body: some View {
        let result = state.result(for: callId)
        let running = state.runningTools.contains(callId)
        Button { showing = true } label: {
            HStack(spacing: 10) {
                Image(systemName: Self.icon(name))
                    .foregroundStyle(result?.isError == true ? .red : .accentColor)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title ?? name).font(.callout).lineLimit(1)
                    if let sub = subtitle(result: result) {
                        Text(sub).font(.caption).foregroundStyle(result?.isError == true ? .red : .secondary)
                            .lineLimit(1).truncationMode(.middle)
                    }
                }
                Spacer(minLength: 4)
                if let ms = result?.durationMs {
                    Text(Self.duration(ms))
                        .font(.caption2.monospaced()).foregroundStyle(.tertiary)
                }
                if running {
                    ProgressView().controlSize(.small)
                } else if let result {
                    Image(systemName: result.isError ? "xmark.circle.fill" : "checkmark.circle.fill")
                        .foregroundStyle(result.isError ? .red : .green)
                }
            }
            .padding(10)
            .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .sheet(isPresented: $showing) {
            ToolDetail(item: .init(name: name, title: title, inputJson: inputJson,
                                   output: result?.output ?? state.liveOutput[callId], isError: result?.isError ?? false))
        }
    }

    static func icon(_ tool: String) -> String {
        switch tool {
        case "shell": "terminal"
        case "file_read": "doc.text"
        case "file_write": "doc.text.fill"
        case "file_edit": "pencil.line"
        case "shell_jobs": "list.bullet.rectangle"
        case "browser": "globe"
        case "read_image": "photo"
        case "device_calendar": "calendar"
        case "device_reminders": "checklist"
        case "device_contacts": "person.crop.circle"
        case "device_location": "location"
        case "device_photos": "photo.on.rectangle"
        case "device_clipboard": "doc.on.clipboard"
        case "device_alarm": "alarm"
        case "device_media": "music.note"
        case "device_health": "heart"
        case "device_weather": "cloud.sun"
        case "device_maps": "map"
        case "device_vision": "text.viewfinder"
        case "device_nlp": "text.magnifyingglass"
        case "device_notify": "bell"
        case "device_open": "arrow.up.forward.app"
        case "device_info": "iphone"
        default: "wrench.and.screwdriver"
        }
    }

    /// Under a second with one decimal, as a person reads it: "0.4s", "12s", "1m 5s".
    static func duration(_ ms: UInt64) -> String {
        let s = Double(ms) / 1000
        if s < 1 { return String(format: "%.1fs", s) }
        if s < 60 { return String(format: "%.0fs", s) }
        return "\(Int(s) / 60)m \(Int(s) % 60)s"
    }

    private func subtitle(result: (output: String, isError: Bool, durationMs: UInt64?)?) -> String? {
        if let result, result.isError, let line = result.output.split(separator: "\n").first { return String(line) }
        if state.runningTools.contains(callId), let live = state.liveOutput[callId],
           let last = live.split(separator: "\n").last(where: { !$0.trimmingCharacters(in: .whitespaces).isEmpty }) {
            return String(last)
        }
        if let data = inputJson.data(using: .utf8),
           let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let line = (obj["command"] ?? obj["path"]) as? String {
            return line.split(separator: "\n").first.map(String.init)
        }
        if let data = inputJson.data(using: .utf8),
           let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let action = obj["action"] as? String {
            // What it did, and to what: a page or element for the browser, the
            // item or search for a device tool.
            let target = ["url", "ref", "selector", "name", "heading", "label", "query"]
                .lazy.compactMap { obj[$0] as? String }.first { !$0.isEmpty }
            return target.map { "\(action) \($0)" } ?? action
        }
        return nil
    }
}

struct ToolDetail: View {
    struct Item {
        let name: String
        let title: String?
        let inputJson: String
        let output: String?
        let isError: Bool
    }

    let item: Item
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    section(String(localized: "Arguments"), arguments)
                    if let output = item.output {
                        section(item.isError ? String(localized: "Error") : String(localized: "Result"), output, error: item.isError)
                    }
                }
                .padding()
            }
            .navigationTitle(item.title ?? item.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button(String(localized: "Done")) { dismiss() } }
            }
        }
    }

    private var arguments: String {
        guard let data = item.inputJson.data(using: .utf8),
              var obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return item.inputJson }
        obj["title"] = nil
        if let command = obj["command"] as? String, obj.count == 1 { return command }
        guard let pretty = try? JSONSerialization.data(withJSONObject: obj, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]) else {
            return item.inputJson
        }
        return String(decoding: pretty, as: UTF8.self)
    }

    private func section(_ label: String, _ text: String, error: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(label).font(.caption).foregroundStyle(.secondary)
            Text(text)
                .font(.caption.monospaced())
                .foregroundStyle(error ? .red : .primary)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
        }
    }
}

extension Part {
    var isThinking: Bool {
        if case .thinking = self { true } else { false }
    }
}

extension Message {
    var text: String {
        parts.compactMap { if case .text(let t) = $0 { t } else { nil } }.joined()
    }
}

/// Where a summary stands in for the messages above it.
private struct CompactionDivider: View {
    let compaction: Compaction
    let undo: () -> Void
    @State private var showing = false

    var body: some View {
        VStack(spacing: 6) {
            HStack {
                VStack { Divider() }
                Text("Earlier messages are summarized for the model").font(.caption).foregroundStyle(.secondary)
                    .fixedSize()
                VStack { Divider() }
            }
            HStack(spacing: 16) {
                Button(String(localized: "Show summary")) { showing = true }
                Button(String(localized: "Undo")) { undo() }
            }
            .font(.caption)
        }
        .padding(.vertical, 6)
        .sheet(isPresented: $showing) {
            NavigationStack {
                ScrollView { MarkdownView(text: compaction.summary).padding() }
                    .navigationTitle(String(localized: "Summary"))
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .confirmationAction) { Button(String(localized: "Done")) { showing = false } }
                    }
            }
        }
    }
}

enum RoomAction { case summarize, newChatOnly }

extension CoreError {
    /// What the chat offers when a turn ran out of room.
    var roomAction: RoomAction? {
        switch self {
        case .ContextNearlyFull(_, _, let canSummarize): canSummarize ? .summarize : .newChatOnly
        case .ContextTooLong: .summarize
        case .NothingToCompact: .newChatOnly
        default: nil
        }
    }
}

private struct ScrollEnds: Equatable {
    var atTop: Bool
    var atBottom: Bool
}

/// Whether the reader is moving the scroll view, and the gap to the end on
/// the last frame. A class: it changes every frame and nothing draws it.
private final class ScrollDrive {
    var byUser = false
    var gap: CGFloat = 0
    /// The top of what is on screen, in content coordinates.
    var visibleTop: CGFloat = 0
    /// Where each question's row starts, in content coordinates.
    var questionTops: [String: CGFloat] = [:]
}

/// Records where a question's row starts, for the step buttons. Other rows
/// are left alone: nothing needs their positions.
private struct QuestionTop: ViewModifier {
    let isQuestion: Bool
    let id: String
    let drive: ScrollDrive

    func body(content: Content) -> some View {
        if isQuestion {
            content.onGeometryChange(for: CGFloat.self) { $0.frame(in: .named(ChatView.contentSpace)).minY } action: { drive.questionTops[id] = $0 }
        } else {
            content
        }
    }
}

/// Under a finished answer: copy it (as its Markdown), answer the question
/// again, or share it. A long press on the text selects part of it, so the
/// whole answer needs targets that are easy to hit.
struct AnswerActions: View {
    let text: String
    let retry: () -> Void
    @State private var copied = false

    var body: some View {
        HStack(spacing: 4) {
            Button {
                UIPasteboard.general.string = text
                copied = true
                Task {
                    try? await Task.sleep(for: .seconds(1.5))
                    copied = false
                }
            } label: {
                icon(copied ? "checkmark" : "doc.on.doc").foregroundStyle(copied ? Color.green : Color.secondary)
            }
            .accessibilityLabel(copied ? String(localized: "Copied") : String(localized: "Copy All"))
            .animation(.easeInOut(duration: 0.15), value: copied)
            Button(action: retry) { icon("arrow.counterclockwise") }
                .accessibilityLabel(String(localized: "Retry"))
            ShareLink(item: text) { icon("square.and.arrow.up") }
                .accessibilityLabel(String(localized: "Share"))
        }
        .buttonStyle(.plain)
        .foregroundStyle(.secondary)
        // The first icon lines up with the text; its frame is wider.
        .padding(.leading, -10)
    }

    private func icon(_ name: String) -> some View {
        Image(systemName: name)
            .font(.footnote)
            .frame(width: 36, height: 28)
            .contentShape(Rectangle())
    }
}
