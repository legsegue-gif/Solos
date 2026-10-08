import SwiftUI

struct SettingsView: View {
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var editing: Endpoint?
    @State private var error: CoreError?
    @State private var sandbox: SandboxStatus?
    /// Read again whenever Settings shows, since it is changed on the
    /// browser's own page.
    @State private var browserAgent = BrowserPrefs.agent
    @State private var browsingFiles = false
    @State private var confirmingClear = false
    @State private var clearError: CoreError?
    /// Read again whenever Settings shows, since it is changed on its page.
    @State private var mirrors: [PackageMirror] = []
    /// Read again whenever Settings shows, since it is changed on its page.
    @State private var skillCount = 0

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    ForEach(app.settings.endpoints, id: \.id) { ep in
                        Button { editing = ep } label: {
                            VStack(alignment: .leading) {
                                Text(ep.name).foregroundStyle(.primary)
                                Text("\(ep.protocol.label) · \(ep.baseUrl.isEmpty ? ep.protocol.officialHost : ep.baseUrl)")
                                    .font(.caption).foregroundStyle(.secondary)
                                // The default model is chosen on this endpoint's
                                // page; shown here so it is visible from the list.
                                if let d = app.settings.defaultModel, d.endpointId == ep.id {
                                    Text("Default: \(d.model)")
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                    .onDelete { offsets in
                        var s = app.settings
                        let removed = offsets.map { s.endpoints[$0] }
                        s.endpoints.remove(atOffsets: offsets)
                        if let d = s.defaultModel, removed.contains(where: { $0.id == d.endpointId }) {
                            s.defaultModel = nil
                        }
                        Task {
                            do {
                                try await app.save(s)
                                removed.forEach { Keychain.write(EndpointEditor.secretRef(for: $0), "") }
                                error = nil
                            } catch let e as CoreError {
                                error = e
                            } catch {}
                        }
                    }
                    Button(String(localized: "Add endpoint")) {
                        editing = Endpoint(id: UUID().uuidString, name: "", protocol: .openAi, baseUrl: "", secretRef: "")
                    }
                    // One switch for every model: with the endpoints, since it is
                    // about the models, not a setting of its own.
                    Toggle(String(localized: "Thinking"), isOn: Binding(
                        get: { app.settings.thinking },
                        set: { on in
                            var s = app.settings
                            s.thinking = on
                            Task { try? await app.save(s) }
                        }))
                } header: {
                    Text("Endpoints")
                } footer: {
                    if let error {
                        Text(error.message).foregroundStyle(.red)
                    } else {
                        Text("Thinking: for new chats, with any model. Each chat can change it from its model menu.")
                    }
                }
                // Headed and valued like the sections around it: the row says
                // what is set, and opens the browser's own settings.
                // Grouped as the home screen's buttons are: files, browser, and
                // the package mirrors the terminal and the model install from.
                filesSection
                Section {
                    NavigationLink { BrowserSettingsView() } label: {
                        LabeledContent("User Agent", value: browserAgent.label)
                    }
                } header: {
                    Text("Browser")
                }
                mirrorsSection
                Section {
                    NavigationLink { SkillsView() } label: {
                        LabeledContent(String(localized: "Skills"), value: skillCount == 0 ? String(localized: "None") : String(skillCount))
                    }
                } header: {
                    Text("Extensions")
                }
            }
            .task { await loadSandbox() }
            .sheet(isPresented: $browsingFiles, onDismiss: { Task { await loadSandbox() } }) { FileBrowser() }
            .confirmationDialog(
                String(localized: "Clear \(Self.size(sandbox?.temporaryBytes ?? 0)) of temporary files?"),
                isPresented: $confirmingClear, titleVisibility: .visible
            ) {
                Button(String(localized: "Clear"), role: .destructive) { Task { await clearTemporary() } }
            }
            .onAppear {
                browserAgent = BrowserPrefs.agent
                mirrors = app.core?.chosenPackageMirrors() ?? []
                skillCount = app.core?.skills().count ?? 0
            }
            .navigationTitle(String(localized: "Settings"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button(String(localized: "Done")) { dismiss() } }
            }
            .sheet(item: $editing) { ep in
                EndpointEditor(endpoint: ep, defaultModel: app.settings.defaultModel)
            }
        }
    }

    /// The room things take, the workspace behind the files button, and
    /// the one thing here that can be thrown away. A sandbox that did not
    /// start says why, at the top.
    @ViewBuilder private var filesSection: some View {
        Section {
            if let error = sandbox?.error {
                Text("Linux system did not start: \(error)").foregroundStyle(.red)
            }
            Button { browsingFiles = true } label: {
                LabeledContent(String(localized: "Workspace")) {
                    HStack(spacing: 6) {
                        Text(sandbox.map { Self.size($0.workspaceBytes) } ?? "…").foregroundStyle(.secondary)
                        Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(.tertiary)
                    }
                }
            }
            .foregroundStyle(.primary)
            if let system = sandbox?.systemBytes {
                LabeledContent(String(localized: "Linux system"), value: Self.size(system))
            }
            if let sandbox {
                LabeledContent(String(localized: "Chats"), value: Self.size(sandbox.databaseBytes))
            }
            Button(role: .destructive) { confirmingClear = true } label: {
                LabeledContent(String(localized: "Clear temporary files"), value: Self.size(sandbox?.temporaryBytes ?? 0))
            }
            .disabled((sandbox?.temporaryBytes ?? 0) == 0)
        } header: {
            Text("Files")
        } footer: {
            if let clearError {
                Text(clearError.message).foregroundStyle(.red)
            } else {
                Text("Temporary files are the pages and screenshots the model saved while browsing. Screenshots in earlier chats will no longer open.")
            }
        }
    }

    /// Where the Linux system installs packages from.
    @ViewBuilder private var mirrorsSection: some View {
        Section {
            ForEach(mirrors, id: \.kind) { m in
                NavigationLink { MirrorsView(kind: m.kind) } label: {
                    LabeledContent(MirrorsView.title(m.kind), value: m.name)
                }
            }
        } header: {
            Text("Mirrors")
        }
    }

    private func loadSandbox() async {
        sandbox = try? await app.core?.sandboxStatus()
    }

    private func clearTemporary() async {
        do {
            _ = try await app.core?.clearTemporaryFiles()
            clearError = nil
        } catch let e as CoreError {
            clearError = e
        } catch {}
        await loadSandbox()
    }

    private static func size(_ bytes: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)
    }
}

extension Endpoint: Identifiable {}

extension Protocol {
    var label: String {
        switch self {
        case .openAi: String(localized: "OpenAI / compatible")
        case .anthropic: String(localized: "Anthropic / compatible")
        case .gemini: String(localized: "Gemini / compatible")
        }
    }

    /// What the address field wants, since each protocol draws the line
    /// between address and path in a different place.
    var addressHelp: String {
        switch self {
        case .openAi: String(localized: "The official API, or any relay or local server that speaks Chat Completions. The address ends before /chat/completions, usually with /v1.")
        case .anthropic: String(localized: "Claude, or any service that speaks the Anthropic Messages API. The address ends before /v1/messages.")
        case .gemini: String(localized: "Gemini, or any service that speaks the Gemini API. The address ends before /models, with the version, as in https://generativelanguage.googleapis.com/v1beta.")
        }
    }

    /// Where requests go when the address is left empty.
    var officialHost: String {
        switch self {
        case .openAi: "api.openai.com"
        case .anthropic: "api.anthropic.com"
        case .gemini: "generativelanguage.googleapis.com"
        }
    }
}

/// One endpoint: where it is, its key, and which of its models is the default.
struct EndpointEditor: View {
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var endpoint: Endpoint
    @State private var key: String
    @State private var models: [ModelInfo] = []
    @State private var chosen: String
    @State private var fetching = false
    @State private var error: CoreError?

    /// The default is read once, here: `onAppear` runs again on coming back
    /// from a model's page and would undo the choice made there.
    init(endpoint: Endpoint, defaultModel: ModelChoice?) {
        _endpoint = State(initialValue: endpoint)
        _key = State(initialValue: Keychain.read(Self.secretRef(for: endpoint)) ?? "")
        _chosen = State(initialValue: defaultModel?.endpointId == endpoint.id ? defaultModel?.model ?? "" : "")
    }

    static func secretRef(for ep: Endpoint) -> String { "endpoint.\(ep.id)" }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(String(localized: "Name"), text: $endpoint.name)
                    Picker(String(localized: "Protocol"), selection: $endpoint.protocol) {
                        ForEach([Protocol.openAi, .anthropic, .gemini], id: \.self) { p in
                            Text(p.label).tag(p)
                        }
                    }
                    .onChange(of: endpoint.protocol) { models = []; chosen = "" }
                    TextField(String(localized: "Address (empty = \(endpoint.protocol.officialHost))"), text: $endpoint.baseUrl)
                        .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
                    SecureField(String(localized: "API key"), text: $key)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                } footer: {
                    Text("\(endpoint.protocol.addressHelp) The key is kept in the Keychain.")
                }
                Section {
                    Button {
                        Task { await fetch() }
                    } label: {
                        HStack {
                            Text("Fetch models")
                            if fetching { Spacer(); ProgressView() }
                        }
                    }
                    .disabled(key.isEmpty || fetching)
                    if let error { Text(error.message).foregroundStyle(.red).font(.caption) }
                }
                if !models.isEmpty {
                    Section {
                        ForEach(models, id: \.id) { m in
                            NavigationLink {
                                ModelDetail(choice: ModelChoice(endpointId: endpoint.id, model: m.id), info: m, chosen: $chosen)
                            } label: {
                                ModelRow(id: m.id, isDefault: chosen == m.id)
                            }
                        }
                    } header: {
                        Text("Models")
                    } footer: {
                        Text("Open a model to make it the default or to set its context window.")
                    }
                }
            }
            .navigationTitle(endpoint.name.isEmpty ? String(localized: "Endpoint") : endpoint.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button(String(localized: "Cancel")) { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button(String(localized: "Save")) { Task { await save() } }
                        .disabled(endpoint.name.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .task {
                // A saved endpoint lists its models at once, as the chat's
                // model menu does; a new one waits for its key.
                if models.isEmpty, !key.isEmpty, app.settings.endpoints.contains(where: { $0.id == endpoint.id }) {
                    await fetch()
                }
            }
        }
    }

    private func fetch() async {
        fetching = true
        defer { fetching = false }
        var ep = endpoint
        ep.secretRef = Self.secretRef(for: ep)
        Keychain.write(ep.secretRef, key)
        do {
            models = try await app.core?.listModels(endpoint: ep) ?? []
            if chosen.isEmpty { chosen = models.first?.id ?? "" }
            error = nil
        } catch let e as CoreError {
            error = e
        } catch {}
    }

    private func save() async {
        var ep = endpoint
        ep.secretRef = Self.secretRef(for: ep)
        Keychain.write(ep.secretRef, key)
        var s = app.settings
        if let i = s.endpoints.firstIndex(where: { $0.id == ep.id }) { s.endpoints[i] = ep } else { s.endpoints.append(ep) }
        if !chosen.isEmpty { s.defaultModel = ModelChoice(endpointId: ep.id, model: chosen) }
        do {
            try await app.save(s)
            dismiss()
        } catch let e as CoreError {
            error = e
        } catch {}
    }
}

/// A model in an endpoint's list: its id, as requests send it.
private struct ModelRow: View {
    let id: String
    let isDefault: Bool

    var body: some View {
        HStack {
            Text(id)
            Spacer()
            if isDefault { Image(systemName: "checkmark").foregroundStyle(.tint) }
        }
    }
}

/// One model: make it the default, or set its context window by hand for an
/// endpoint that reports none or reports it wrong.
struct ModelDetail: View {
    let choice: ModelChoice
    let info: ModelInfo
    @Binding var chosen: String
    @Environment(AppCore.self) private var app
    @State private var windowText = ""
    @State private var reported: UInt64?
    @State private var error: CoreError?

    var body: some View {
        Form {
            Section {
                Button {
                    chosen = choice.model
                } label: {
                    HStack {
                        Text("Default model")
                        Spacer()
                        if chosen == choice.model { Image(systemName: "checkmark").foregroundStyle(.tint) }
                    }
                }
            } footer: {
                Text("Used for new chats once the endpoint is saved.")
            }
            Section {
                HStack {
                    Text("Context window")
                    Spacer()
                    TextField(reported.map { "\($0)" } ?? String(localized: "Unknown"), text: $windowText)
                        .keyboardType(.numberPad)
                        .multilineTextAlignment(.trailing)
                        .monospacedDigit()
                }
            } footer: {
                if let error {
                    Text(error.message).foregroundStyle(.red)
                } else if let reported {
                    Text("Tokens. Leave empty to use what the endpoint reports (\(reported)). Near the limit, old tool output is cleared and the chat asks before summarising.")
                } else {
                    Text("Tokens. The endpoint does not report this model's window; without one, a chat only learns it is too long when the endpoint refuses.")
                }
            }
        }
        .navigationTitle(info.id)
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            let w = app.core?.modelWindow(model: choice)
            reported = w?.reported ?? info.contextWindow
            windowText = w?.custom.map { "\($0)" } ?? ""
        }
        .onDisappear { Task { await save() } }
    }

    private func save() async {
        let digits = windowText.filter(\.isNumber)
        do {
            try await app.core?.setModelWindow(model: choice, window: UInt64(digits))
        } catch let e as CoreError {
            error = e
        } catch {}
    }
}
