import SwiftUI
import UniformTypeIdentifiers

/// The MCP servers: each can be turned off, opened to see and change it,
/// and deleted. The model adds them the same way when asked in a chat.
struct McpServersView: View {
    @Environment(AppCore.self) private var app
    @State private var servers: [McpServer] = []
    @State private var adding = false
    @State private var error: CoreError?

    var body: some View {
        Form {
            Section {
                ForEach(servers, id: \.name) { server in
                    ExtensionRow(
                        title: server.name,
                        subtitle: Self.target(server),
                        status: server.error ?? Self.toolCount(server.tools.count),
                        failed: server.error != nil,
                        enabled: Binding(get: { server.enabled }, set: { on in Task { await setEnabled(server, on) } })
                    ) {
                        McpServerDetailView(name: server.name)
                    }
                }
                if servers.isEmpty {
                    Text("No MCP servers yet.").foregroundStyle(.secondary)
                }
            } footer: {
                if let error {
                    Text(error.message).foregroundStyle(.red)
                } else {
                    Text("An MCP server gives the model more tools. You can also ask in a chat: “add this MCP server: <link>”.")
                }
            }
        }
        .navigationTitle(String(localized: "MCP servers"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { adding = true } label: { Image(systemName: "plus") }
                    .accessibilityLabel(String(localized: "Add MCP server"))
            }
        }
        .sheet(isPresented: $adding, onDismiss: load) { McpAddSheet() }
        .onAppear { load() }
    }

    /// The address, or the command line, a server runs as.
    static func target(_ server: McpServer) -> String {
        server.url.isEmpty ? ([server.command] + server.args).joined(separator: " ") : server.url
    }

    static func toolCount(_ n: Int) -> String {
        n == 1 ? String(localized: "1 tool") : String(localized: "\(n) tools")
    }

    private func load() {
        servers = app.core?.mcpServers() ?? []
    }

    private func setEnabled(_ server: McpServer, _ on: Bool) async {
        do {
            try await app.core?.setMcpServerEnabled(name: server.name, enabled: on)
            error = nil
        } catch let e as CoreError {
            error = e
        } catch {}
        load()
    }
}

/// A new server from a remote address, pasted `mcpServers` JSON, or a JSON
/// file. Each is connected to as it is added, and kept with what went wrong
/// when that fails.
private struct McpAddSheet: View {
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var source = AddSource.link
    @State private var address = ""
    @State private var name = ""
    @State private var nameEdited = false
    @State private var authorization = ""
    @State private var json = ""
    @State private var picking = false
    @State private var adding = false
    @State private var error: CoreError?
    @State private var failed: [McpServer] = []

    var body: some View {
        NavigationStack {
            Form {
                Section { AddSourcePicker(source: $source) }
                switch source {
                case .link:
                    Section {
                        TextField("https://example.com/mcp", text: $address)
                            .keyboardType(.URL)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .onChange(of: address) { if !nameEdited { name = Self.name(for: address) } }
                        TextField(String(localized: "Name"), text: Binding(get: { name }, set: { name = $0; nameEdited = true }))
                            .keyboardType(.URL)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                        // Not a secure field: one makes iOS offer Passwords on
                        // every field of the sheet.
                        TextField(String(localized: "Authorization (optional)"), text: $authorization)
                            .keyboardType(.URL)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    } footer: {
                        Text("A remote server's address. Authorization is sent as that header, for example “Bearer <key>”, and kept in the Keychain. For a server that runs on this device, paste its JSON from the README.")
                    }
                case .paste:
                    Section {
                        PasteEditor(text: $json, straightQuotes: true)
                    } footer: {
                        Text("The mcpServers JSON from the server's README, for example {\"mcpServers\": {\"12306-mcp\": {\"command\": \"npx\", \"args\": [\"-y\", \"12306-mcp\"]}}}. A command runs in the Linux system, which needs what it uses installed (apk add nodejs npm); a first start can take ten minutes.")
                    }
                case .file:
                    Section {
                        Button(String(localized: "Choose File…")) { picking = true }
                            .disabled(adding)
                    } footer: {
                        Text("A JSON file with mcpServers in it.")
                    }
                }
                if adding {
                    Section {
                        HStack { Text("Connecting…").foregroundStyle(.secondary); Spacer(); ProgressView() }
                    }
                }
                if let error {
                    Section { Text(error.message).foregroundStyle(.red) }
                }
                if !failed.isEmpty {
                    Section {
                        ForEach(failed, id: \.name) { s in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(s.name)
                                Text(s.error ?? "").font(.caption).foregroundStyle(.red)
                            }
                        }
                    } header: {
                        Text("Added, but could not start")
                    }
                }
            }
            .navigationTitle(String(localized: "Add MCP server"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(failed.isEmpty ? String(localized: "Cancel") : String(localized: "Done")) { dismiss() }
                }
                if source != .file {
                    ToolbarItem(placement: .confirmationAction) {
                        Button(String(localized: "Add")) { Task { await add() } }
                            .disabled(adding || !ready)
                    }
                }
            }
            .interactiveDismissDisabled(adding)
            .onChange(of: source) { error = nil }
            .fileImporter(isPresented: $picking, allowedContentTypes: [.json, .plainText, .text]) { result in
                guard case .success(let url) = result else { return }
                Task {
                    do {
                        let text = try String(contentsOf: try PickedFile.copy(url), encoding: .utf8)
                        await run { try await $0.addMcpServers(json: text) }
                    } catch {
                        self.error = .NotAnMcpConfig(detail: error.localizedDescription)
                    }
                }
            }
        }
    }

    private var ready: Bool {
        switch source {
        case .link: !address.trimmingCharacters(in: .whitespaces).isEmpty && !name.trimmingCharacters(in: .whitespaces).isEmpty
        case .paste: !json.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        case .file: false
        }
    }

    /// A name from an address: its host's first part (`mcp.example.com` →
    /// `example`, `api.github.com` → `github`).
    static func name(for address: String) -> String {
        guard let host = URL(string: address.trimmingCharacters(in: .whitespaces))?.host else { return "" }
        let parts = host.split(separator: ".").map(String.init).filter { !["www", "mcp", "api"].contains($0) }
        return parts.count >= 2 ? parts[parts.count - 2] : (parts.first ?? host)
    }

    private func add() async {
        switch source {
        case .link:
            let auth = authorization.trimmingCharacters(in: .whitespaces)
            let server = McpServer(
                name: name.trimmingCharacters(in: .whitespaces), url: address.trimmingCharacters(in: .whitespaces),
                headers: auth.isEmpty ? [:] : ["Authorization": auth], command: "", args: [], env: [:],
                enabled: true, tools: [], error: nil)
            await run { [try await $0.addMcpServer(server: server)] }
        case .paste:
            await run { try await $0.addMcpServers(json: json) }
        case .file:
            break
        }
    }

    private func run(_ add: (SolosCore) async throws -> [McpServer]) async {
        guard let core = app.core else { return }
        adding = true
        defer { adding = false }
        do {
            let added = try await add(core)
            error = nil
            failed = added.filter { $0.error != nil }
            if failed.isEmpty { dismiss() }
        } catch let e as CoreError {
            error = e
        } catch {}
    }
}

/// One server: its state and tools, everything about how it runs (which can
/// be changed and saved), its switch, and deleting it.
struct McpServerDetailView: View {
    @State var name: String
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var server: McpServer?
    @State private var draftName = ""
    @State private var address = ""
    @State private var command = ""
    @State private var arguments = ""
    @State private var pairs: [KeyValue] = []
    @State private var working = false
    @State private var error: CoreError?
    @State private var confirmingDelete = false

    var body: some View {
        Form {
            if let server {
                Section {
                    if let failure = server.error {
                        Text(failure).font(.caption).foregroundStyle(.red).textSelection(.enabled)
                    }
                    Button {
                        Task { await connect() }
                    } label: {
                        HStack {
                            Text("Connect again")
                            if working { Spacer(); ProgressView() }
                        }
                    }
                    .disabled(working)
                } header: {
                    Text(McpServersView.toolCount(server.tools.count))
                } footer: {
                    if let error { Text(error.message).foregroundStyle(.red) }
                }
                Section {
                    field(String(localized: "Name"), $draftName)
                    if isRemote {
                        field("https://example.com/mcp", $address)
                    } else {
                        field(String(localized: "Command"), $command)
                        TextField(String(localized: "Arguments, one per line"), text: $arguments, axis: .vertical)
                            .font(.footnote.monospaced())
                            .keyboardType(.URL)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    }
                } header: {
                    Text(isRemote ? String(localized: "Remote server") : String(localized: "Runs in the Linux system"))
                }
                Section {
                    KeyValueEditor(pairs: $pairs, keyPlaceholder: isRemote ? String(localized: "Header") : String(localized: "Variable"))
                } header: {
                    Text(isRemote ? String(localized: "Headers") : String(localized: "Environment"))
                } footer: {
                    Text("Values are kept in the Keychain.")
                }
                if !server.tools.isEmpty {
                    Section {
                        ForEach(server.tools, id: \.name) { tool in
                            VStack(alignment: .leading, spacing: 2) {
                                Text(tool.name).font(.body.monospaced())
                                if !tool.description.isEmpty {
                                    Text(tool.description).font(.caption).foregroundStyle(.secondary).lineLimit(4)
                                }
                            }
                        }
                    } header: {
                        Text("Tools")
                    }
                }
                Section {
                    Toggle(String(localized: "On"), isOn: Binding(
                        get: { server.enabled },
                        set: { on in Task { await setEnabled(on) } }))
                }
                Section {
                    Button(String(localized: "Delete Server"), role: .destructive) { confirmingDelete = true }
                }
            }
        }
        .navigationTitle(name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                if changed {
                    Button(String(localized: "Save")) { Task { await save() } }
                        .disabled(working)
                }
            }
        }
        .confirmationDialog(
            String(localized: "Delete the MCP server “\(name)”?"),
            isPresented: $confirmingDelete, titleVisibility: .visible
        ) {
            Button(String(localized: "Delete"), role: .destructive) { Task { await remove() } }
        }
        .onAppear { load() }
    }

    private var isRemote: Bool { !(server?.url.isEmpty ?? true) }

    private func field(_ placeholder: String, _ text: Binding<String>) -> some View {
        TextField(placeholder, text: text)
            .font(.body.monospaced())
            .keyboardType(.URL)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
    }

    /// The server as the fields now describe it.
    private var draft: McpServer? {
        guard var s = server else { return nil }
        s.name = draftName.trimmingCharacters(in: .whitespaces)
        if isRemote {
            s.url = address.trimmingCharacters(in: .whitespaces)
            s.headers = KeyValue.map(pairs)
        } else {
            s.command = command.trimmingCharacters(in: .whitespaces)
            s.args = arguments.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
            s.env = KeyValue.map(pairs)
        }
        return s
    }

    private var changed: Bool {
        guard let server, let draft else { return false }
        return draft.name != server.name || draft.url != server.url || draft.command != server.command
            || draft.args != server.args || draft.headers != server.headers || draft.env != server.env
    }

    private func load() {
        server = app.core?.mcpServers().first { $0.name == name }
        guard let server else { return }
        draftName = server.name
        address = server.url
        command = server.command
        arguments = server.args.joined(separator: "\n")
        pairs = KeyValue.from(server.url.isEmpty ? server.env : server.headers)
    }

    private func save() async {
        guard let draft else { return }
        await work { try await $0.updateMcpServer(name: name, server: draft) }
    }

    private func connect() async {
        await work { try await $0.refreshMcpServer(name: name) }
    }

    private func work(_ run: (SolosCore) async throws -> McpServer?) async {
        guard let core = app.core else { return }
        working = true
        defer { working = false }
        do {
            if let saved = try await run(core) { name = saved.name }
            error = nil
        } catch let e as CoreError {
            error = e
        } catch {}
        load()
    }

    private func setEnabled(_ on: Bool) async {
        do {
            try await app.core?.setMcpServerEnabled(name: name, enabled: on)
        } catch let e as CoreError {
            error = e
        } catch {}
        load()
    }

    private func remove() async {
        do {
            try await app.core?.removeMcpServer(name: name)
            dismiss()
        } catch let e as CoreError {
            error = e
        } catch {}
    }
}
