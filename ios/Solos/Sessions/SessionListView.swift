import SwiftUI

struct SessionListView: View {
    @Environment(AppCore.self) private var app
    @State private var path: [Route] = []
    @State private var showingSettings = false
    @State private var showingAlarms = false
    @State private var showingFiles = false
    @State private var showingTerminal = false
    @State private var showingBrowser = false
    @ObservedObject private var alarms = AlarmStore.shared
    @Environment(\.scenePhase) private var scenePhase
    @State private var query = ""
    /// Search results; nil when not searching.
    @State private var found: [SessionInfo]?
    @State private var renaming: SessionInfo?
    @State private var newTitle = ""
    @State private var error: CoreError?
    @State private var selecting = false
    @State private var selection = Set<String>()
    @State private var confirmingDelete = false
    @State private var sharing: SharedFiles?

    /// Exported chats handed to the share sheet.
    struct SharedFiles: Identifiable {
        let id = UUID()
        let urls: [URL]
    }

    enum ExportFormat { case json, text }

    /// Each new chat gets its own route: replacing one chat with another
    /// at the same place in the stack must give a fresh view, and two new
    /// chats in a row must differ.
    enum Route: Hashable {
        case chat(String)
        case newChat(UUID)
    }

    var body: some View {
        NavigationStack(path: $path) {
            List(selection: $selection) {
                if let error = app.startupError ?? error {
                    Section {
                        Text(error.message).foregroundStyle(.red)
                    }
                }
                if let found {
                    ForEach(found, id: \.id) { row($0) }
                } else {
                    ForEach(Array(SessionGroup.make(app.sessions).enumerated()), id: \.element.group) { index, g in
                        Section {
                            ForEach(g.sessions, id: \.id) { row($0) }
                        } header: {
                            // The newest group needs no heading unless it is
                            // the pinned one, as in the reference app.
                            if index > 0 || g.group == .pinned {
                                Label {
                                    Text(g.group.title)
                                } icon: {
                                    if g.group == .pinned { Image(systemName: "pin.fill").font(.caption2) }
                                }
                                .font(.subheadline.weight(.semibold))
                                .foregroundStyle(.secondary)
                                .textCase(nil)
                            }
                        }
                    }
                }
            }
            .listStyle(.plain)
            .environment(\.editMode, .constant(selecting ? .active : .inactive))
            .overlay {
                if found?.isEmpty == true {
                    ContentUnavailableView.search(text: query)
                } else if app.sessions.isEmpty && app.startupError == nil {
                    ContentUnavailableView(String(localized: "No chats yet"), systemImage: "bubble.left.and.bubble.right")
                }
            }
            .navigationTitle("Solos")
            .navigationBarTitleDisplayMode(.inline)
            // No title on screen: the buttons need the room, and the list is
            // the app's home. The title stays for the back button's menu.
            .toolbar(removing: .title)
            .toolbar {
                if selecting {
                    ToolbarItem(placement: .topBarLeading) {
                        Button(String(localized: "Done")) { selecting = false; selection = [] }
                    }
                    ToolbarItem(placement: .topBarTrailing) {
                        let all = Set((found ?? app.sessions).map(\.id))
                        Button(selection == all ? String(localized: "Deselect All") : String(localized: "Select All")) {
                            selection = selection == all ? [] : all
                        }
                    }
                }
            }
            .toolbar {
                if !selecting {
                ToolbarItemGroup(placement: .topBarLeading) {
                    Button { showingSettings = true } label: {
                        Label(String(localized: "Settings"), systemImage: "gearshape")
                    }
                    Button { showingFiles = true } label: {
                        Label(String(localized: "Browse files"), systemImage: "folder")
                    }
                    .disabled(app.core == nil)
                    Button { showingTerminal = true } label: {
                        Label(String(localized: "Open terminal"), systemImage: "apple.terminal")
                    }
                    .disabled(app.core == nil)
                    Button { showingBrowser = true } label: {
                        Label(String(localized: "Open browser"), systemImage: "globe")
                    }
                    .disabled(app.core == nil)
                }
                // Only while an alarm is pending: the button appearing is
                // itself the sign that one was set.
                if !alarms.alarms.isEmpty {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button { showingAlarms = true } label: {
                            Label(String(localized: "Alarms"), systemImage: "alarm")
                        }
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { path.append(.newChat(UUID())) } label: {
                        Label(String(localized: "New chat"), systemImage: "square.and.pencil")
                    }
                    .disabled(app.core == nil)
                }
                }
            }
            .safeAreaInset(edge: .bottom) {
                if selecting { selectionBar }
            }
            .sheet(item: $sharing) { ShareSheet(items: $0.urls) }
            .confirmationDialog(
                String(localized: "Delete \(selection.count) chats?"),
                isPresented: $confirmingDelete, titleVisibility: .visible
            ) {
                Button(String(localized: "Delete"), role: .destructive) {
                    delete(Array(selection))
                    selecting = false
                    selection = []
                }
            }
            .navigationDestination(for: Route.self) { route in
                switch route {
                case .chat(let id): ChatView(sessionId: id).id(route)
                case .newChat: ChatView(sessionId: nil).id(route)
                }
            }
            .sheet(isPresented: $showingSettings) {
                SettingsView()
            }
            .sheet(isPresented: $showingAlarms) { AlarmListView() }
            .sheet(isPresented: $showingFiles) { FileBrowser() }
            .fullScreenCover(isPresented: $showingTerminal) { TerminalScreen() }
            .sheet(isPresented: $showingBrowser) { NavigationStack { BrowserView() } }
            .onAppear { alarms.refresh() }
            // An alarm may have gone off and been dismissed meanwhile.
            .onChange(of: scenePhase) { _, phase in if phase == .active { alarms.refresh() } }
            .refreshable { await app.reloadSessions() }
            .onChange(of: app.openSessionRequested) { _, id in
                guard let id else { return }
                app.openSessionRequested = nil
                path = [.chat(id)]
            }
            .onChange(of: app.newChatRequested) { _, requested in
                guard requested else { return }
                app.newChatRequested = false
                path = [.newChat(UUID())]
            }
            .searchable(text: $query, prompt: String(localized: "Search chats"))
            .task(id: query) { await search() }
            .onChange(of: app.sessions) { Task { await search() } }
            .alert(String(localized: "Rename chat"), isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
                TextField(String(localized: "Title"), text: $newTitle)
                Button(String(localized: "Cancel"), role: .cancel) {}
                Button(String(localized: "Save")) {
                    guard let id = renaming?.id else { return }
                    Task {
                        await report { _ = try await app.core?.renameSession(sessionId: id, title: newTitle) }
                        await app.reloadSessions()
                    }
                }
            }
        }
    }

    private func row(_ s: SessionInfo) -> some View {
        // A button rather than a link: no disclosure arrow, as in the
        // reference app, and in selection mode a tap selects instead.
        Button {
            if selecting {
                if selection.contains(s.id) { selection.remove(s.id) } else { selection.insert(s.id) }
            } else {
                path.append(.chat(s.id))
            }
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(s.title ?? s.preview ?? String(localized: "New chat"))
                        .font(.body.weight(.medium))
                        .lineLimit(1)
                    if let preview = s.preview, s.title != nil {
                        Text(preview).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
                    }
                }
                Spacer(minLength: 8)
                Text(s.activityLabel()).font(.caption).foregroundStyle(.tertiary)
            }
            .padding(.vertical, 4)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .tag(s.id)
        .swipeActions {
            Button(role: .destructive) { delete([s.id]) } label: {
                Label(String(localized: "Delete"), systemImage: "trash")
            }
        }
        .contextMenu { menu(s) }
    }

    @ViewBuilder private func menu(_ s: SessionInfo) -> some View {
        let pinned = s.pinnedAt != nil
        Button {
            Task {
                await report { _ = try await app.core?.setPinned(sessionId: s.id, pinned: !pinned) }
                await app.reloadSessions()
            }
        } label: {
            Label(pinned ? String(localized: "Unpin") : String(localized: "Pin"), systemImage: pinned ? "pin.slash" : "pin")
        }
        Menu {
            Button { export([s.id], as: .json) } label: { Label("JSON", systemImage: "doc.text") }
            Button { export([s.id], as: .text) } label: { Label(String(localized: "Plain Text"), systemImage: "text.alignleft") }
        } label: {
            Label(String(localized: "Export"), systemImage: "square.and.arrow.up")
        }
        Button {
            newTitle = s.title ?? ""
            renaming = s
        } label: { Label(String(localized: "Rename"), systemImage: "pencil") }
        Button {
            Task {
                await report { _ = try await app.core?.regenerateTitle(sessionId: s.id) }
                await app.reloadSessions()
            }
        } label: {
            Label(String(localized: "Regenerate Title"), systemImage: "arrow.triangle.2.circlepath")
        }
        Button {
            let name = s.title ?? s.preview ?? String(localized: "New chat")
            Task {
                await report { _ = try await app.core?.duplicateSession(sessionId: s.id, title: String(localized: "\(name) (Copy)")) }
                await app.reloadSessions()
            }
        } label: {
            Label(String(localized: "Duplicate"), systemImage: "plus.square.on.square")
        }
        Button {
            selection = [s.id]
            selecting = true
        } label: {
            Label(String(localized: "Select"), systemImage: "checkmark.circle")
        }
        Divider()
        Button(role: .destructive) { delete([s.id]) } label: {
            Label(String(localized: "Delete"), systemImage: "trash")
        }
    }

    /// What can be done to the chats picked in selection mode.
    private var selectionBar: some View {
        HStack {
            Menu {
                Button { export(Array(selection), as: .json) } label: { Label("JSON", systemImage: "doc.text") }
                Button { export(Array(selection), as: .text) } label: { Label(String(localized: "Plain Text"), systemImage: "text.alignleft") }
            } label: {
                Label(String(localized: "Export"), systemImage: "square.and.arrow.up")
            }
            Spacer()
            Text(String(localized: "\(selection.count) selected")).font(.subheadline).foregroundStyle(.secondary)
            Spacer()
            Button(role: .destructive) { confirmingDelete = true } label: {
                Label(String(localized: "Delete"), systemImage: "trash")
            }
        }
        .disabled(selection.isEmpty)
        .padding(.horizontal, 20)
        .padding(.vertical, 12)
        .background(.bar)
    }

    /// Each chat as a file named after it, then the share sheet.
    private func export(_ ids: [String], as format: ExportFormat) {
        guard let core = app.core else { return }
        Task {
            let dir = FileManager.default.temporaryDirectory.appendingPathComponent("export-\(UUID().uuidString)", isDirectory: true)
            var urls: [URL] = []
            await report {
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                for id in ids {
                    let info = (found ?? app.sessions).first { $0.id == id }
                    let body = format == .json ? try await core.exportSession(sessionId: id) : try await core.exportSessionText(sessionId: id)
                    let base = (info?.title ?? info?.preview ?? id)
                        .components(separatedBy: CharacterSet(charactersIn: "/\\:?*\"<>|\n")).joined(separator: " ")
                        .trimmingCharacters(in: .whitespaces).prefix(80)
                    var url = dir.appendingPathComponent("\(base).\(format == .json ? "json" : "txt")")
                    if urls.contains(url) { url = dir.appendingPathComponent("\(base) \(id.prefix(6)).\(format == .json ? "json" : "txt")") }
                    try body.write(to: url, atomically: true, encoding: .utf8)
                    urls.append(url)
                }
            }
            if !urls.isEmpty { sharing = SharedFiles(urls: urls) }
        }
    }

    private func delete(_ ids: [String]) {
        Task {
            for id in ids { await report { try await app.core?.deleteSession(sessionId: id) } }
            await app.reloadSessions()
        }
    }

    /// Title and message text, searched by the core; typing pauses briefly
    /// before each search.
    private func search() async {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { found = nil; return }
        try? await Task.sleep(for: .milliseconds(200))
        guard !Task.isCancelled else { return }
        await report { found = try await app.core?.searchSessions(query: q) ?? found }
    }

    /// Run a core call; a failure is shown above the list.
    private func report(_ body: () async throws -> Void) async {
        do {
            try await body()
            error = nil
        } catch let e as CoreError {
            error = e
        } catch {}
    }
}
