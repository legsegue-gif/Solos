import SwiftUI

/// The MCP servers: each can be turned off, opened to see its tools, and
/// deleted; new ones come from pasted `mcpServers` JSON. The model adds them
/// the same way when asked in a chat.
struct McpServersView: View {
    @Environment(AppCore.self) private var app
    @State private var servers: [McpServer] = []
    @State private var adding = false
    @State private var error: CoreError?

    var body: some View {
        Form {
            Section {
                ForEach(servers, id: \.name) { server in
                    row(server)
                }
                if servers.isEmpty {
                    Text("No MCP servers yet.").foregroundStyle(.secondary)
                }
            } footer: {
                if let error {
                    Text(error.message).foregroundStyle(.red)
                } else {
                    Text("An MCP server gives the model more tools. Paste the mcpServers JSON from the server's README, or ask in a chat: “add this MCP server: <link>”. A server's command (npx, uvx …) runs in the Linux system, which needs what it uses installed (apk add nodejs npm).")
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

    private func row(_ server: McpServer) -> some View {
        HStack {
            NavigationLink { McpServerDetailView(name: server.name) } label: {
                VStack(alignment: .leading, spacing: 2) {
                    Text(server.name).foregroundStyle(server.enabled ? Color.primary : Color.secondary)
                    Text(Self.target(server)).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1)
                    if let failure = server.error {
                        Text(failure).font(.caption).foregroundStyle(.red).lineLimit(2)
                    } else {
                        Text(Self.toolCount(server.tools.count)).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            Toggle(String(localized: "On"), isOn: Binding(
                get: { server.enabled },
                set: { on in Task { await setEnabled(server, on) } }))
                .labelsHidden()
                .fixedSize()
        }
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

/// Paste a server's config; each server in it is connected to as it is
/// added, and kept with what went wrong when that fails.
private struct McpAddSheet: View {
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var json = ""
    @State private var adding = false
    @State private var error: CoreError?
    @State private var failed: [McpServer] = []

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    // The URL keyboard types straight quotes; the default one
                    // makes them curly, which is not JSON.
                    TextEditor(text: $json)
                        .font(.system(.footnote, design: .monospaced))
                        .frame(minHeight: 180)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } footer: {
                    if let error {
                        Text(error.message).foregroundStyle(.red)
                    } else {
                        Text("For example: {\"mcpServers\": {\"12306-mcp\": {\"command\": \"npx\", \"args\": [\"-y\", \"12306-mcp\"]}}}. Starting a server for the first time can take a few minutes.")
                    }
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
                ToolbarItem(placement: .confirmationAction) {
                    if adding {
                        ProgressView()
                    } else {
                        Button(String(localized: "Add")) { Task { await add() } }
                            .disabled(json.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
            }
            .interactiveDismissDisabled(adding)
        }
    }

    private func add() async {
        adding = true
        defer { adding = false }
        do {
            let added = try await app.core?.addMcpServers(json: json) ?? []
            error = nil
            failed = added.filter { $0.error != nil }
            if failed.isEmpty { dismiss() }
        } catch let e as CoreError {
            error = e
        } catch {}
    }
}

/// One server: how it runs, its tools, and what went wrong last.
struct McpServerDetailView: View {
    let name: String
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var server: McpServer?
    @State private var refreshing = false
    @State private var error: CoreError?
    @State private var confirmingDelete = false

    var body: some View {
        Form {
            if let server {
                Section {
                    Text(McpServersView.target(server)).font(.footnote.monospaced()).textSelection(.enabled)
                    // Values can be keys; only their names are shown.
                    let keys = (server.url.isEmpty ? server.env.keys : server.headers.keys).sorted()
                    if !keys.isEmpty {
                        LabeledContent(server.url.isEmpty ? String(localized: "Environment") : String(localized: "Headers"), value: keys.joined(separator: ", "))
                    }
                    if let failure = server.error {
                        Text(failure).font(.caption).foregroundStyle(.red).textSelection(.enabled)
                    }
                    Button {
                        Task { await refresh() }
                    } label: {
                        HStack {
                            Text("Connect again")
                            if refreshing { Spacer(); ProgressView() }
                        }
                    }
                    .disabled(refreshing)
                } footer: {
                    if let error { Text(error.message).foregroundStyle(.red) }
                }
                Section {
                    ForEach(server.tools, id: \.name) { tool in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(tool.name).font(.body.monospaced())
                            if !tool.description.isEmpty {
                                Text(tool.description).font(.caption).foregroundStyle(.secondary).lineLimit(4)
                            }
                        }
                    }
                    if server.tools.isEmpty {
                        Text("No tools listed yet.").foregroundStyle(.secondary)
                    }
                } header: {
                    Text(McpServersView.toolCount(server.tools.count))
                }
            }
        }
        .navigationTitle(name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button(role: .destructive) { confirmingDelete = true } label: { Image(systemName: "trash") }
                    .accessibilityLabel(String(localized: "Delete"))
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

    private func load() {
        server = app.core?.mcpServers().first { $0.name == name }
    }

    private func refresh() async {
        refreshing = true
        defer { refreshing = false }
        do {
            server = try await app.core?.refreshMcpServer(name: name)
            error = nil
        } catch let e as CoreError {
            error = e
        } catch {}
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
