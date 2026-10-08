import SwiftUI
import UniformTypeIdentifiers

/// The installed skills. Each is a folder in the workspace's `skills`; the
/// model installs them the same ways when asked in a chat.
struct SkillsView: View {
    @Environment(AppCore.self) private var app
    @State private var skills: [Skill] = []
    @State private var adding = false
    @State private var error: CoreError?

    var body: some View {
        Form {
            Section {
                ForEach(skills, id: \.folder) { skill in
                    ExtensionRow(
                        title: skill.name,
                        subtitle: skill.description,
                        status: Self.status(skill),
                        enabled: Binding(get: { skill.enabled }, set: { on in Task { await setEnabled(skill, on) } })
                    ) {
                        SkillDetailView(folder: skill.folder)
                    }
                }
                if skills.isEmpty {
                    Text("No skills yet.").foregroundStyle(.secondary)
                }
            } footer: {
                if let error {
                    Text(SkillAddSheet.message(error)).foregroundStyle(.red)
                } else {
                    Text("A skill is a folder of instructions, and often scripts, that the model reads when a task calls for it. You can also ask in a chat: “install this skill: <link>”. Skills are kept in the workspace's skills folder.")
                }
            }
        }
        .navigationTitle(String(localized: "Skills"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { adding = true } label: { Image(systemName: "plus") }
                    .accessibilityLabel(String(localized: "Add skill"))
            }
        }
        .sheet(isPresented: $adding, onDismiss: load) { SkillAddSheet() }
        .onAppear { load() }
    }

    /// Its files, and where it came from.
    static func status(_ skill: Skill) -> String {
        let files = skill.fileCount == 1 ? String(localized: "1 file") : String(localized: "\(Int(skill.fileCount)) files")
        return skill.source == nil ? files : "\(files) · GitHub"
    }

    private func load() {
        skills = app.core?.skills() ?? []
    }

    private func setEnabled(_ skill: Skill, _ on: Bool) async {
        do {
            try await app.core?.setSkillEnabled(folder: skill.folder, enabled: on)
            error = nil
        } catch let e as CoreError {
            error = e
        } catch {}
        load()
    }
}

/// A new skill from a GitHub link, a pasted SKILL.md, or a file (`.zip`,
/// `.skill`, `SKILL.md`, or a folder).
struct SkillAddSheet: View {
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var source = AddSource.link
    @State private var link = ""
    @State private var text = ""
    @State private var picking = false
    @State private var installing = false
    @State private var error: CoreError?

    var body: some View {
        NavigationStack {
            Form {
                Section { AddSourcePicker(source: $source) }
                switch source {
                case .link:
                    Section {
                        TextField("https://github.com/owner/repo", text: $link)
                            .keyboardType(.URL)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    } footer: {
                        Text("A GitHub repository, or the folder in one that holds the SKILL.md.")
                    }
                case .paste:
                    Section {
                        PasteEditor(text: $text, straightQuotes: true)
                    } footer: {
                        Text("The whole SKILL.md, with `name` and `description` at the top.")
                    }
                case .file:
                    Section {
                        Button(String(localized: "Choose File…")) { picking = true }
                            .disabled(installing)
                    } footer: {
                        Text("A .zip or .skill file, a SKILL.md, or a folder with one.")
                    }
                }
                if installing {
                    Section {
                        HStack { Text("Installing…").foregroundStyle(.secondary); Spacer(); ProgressView() }
                    }
                }
                if let error {
                    Section {
                        Text(Self.message(error)).foregroundStyle(.red)
                    }
                    // A repository with several skills: each, to choose from.
                    if case .NoSingleSkill(_, let candidates) = error, candidates.contains(where: { $0.hasPrefix("https://") }) {
                        Section {
                            ForEach(candidates, id: \.self) { candidate in
                                Button(candidate.split(separator: "/").last.map(String.init) ?? candidate) {
                                    Task { await install { try await $0.installSkill(source: candidate) } }
                                }
                                .disabled(installing)
                            }
                        } header: {
                            Text("Skills in this repository")
                        }
                    }
                }
            }
            .navigationTitle(String(localized: "Add skill"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(String(localized: "Cancel")) { dismiss() }
                }
                if source != .file {
                    ToolbarItem(placement: .confirmationAction) {
                        Button(String(localized: "Add")) { Task { await add() } }
                            .disabled(installing || current.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
            }
            .interactiveDismissDisabled(installing)
            .onChange(of: source) { error = nil }
            .fileImporter(isPresented: $picking, allowedContentTypes: Self.fileTypes) { result in
                guard case .success(let url) = result else { return }
                Task {
                    await install { core in
                        let copy = try PickedFile.copy(url)
                        return try await core.installSkillFile(path: copy.path)
                    }
                }
            }
        }
    }

    private var current: String { source == .link ? link : text }

    static let fileTypes: [UTType] = [.zip, .folder, .plainText, .text]
        + [UTType(filenameExtension: "skill"), UTType(filenameExtension: "md")].compactMap { $0 }

    private func add() async {
        let value = current.trimmingCharacters(in: .whitespacesAndNewlines)
        switch source {
        case .link: await install { try await $0.installSkill(source: value) }
        case .paste: await install { try await $0.installSkillText(text: value) }
        case .file: break
        }
    }

    private func install(_ run: (SolosCore) async throws -> Skill) async {
        guard let core = app.core else { return }
        installing = true
        defer { installing = false }
        do {
            _ = try await run(core)
            error = nil
            dismiss()
        } catch let e as CoreError {
            error = e
        } catch {
            self.error = .Storage(detail: error.localizedDescription)
        }
    }

    /// Download failures here are GitHub's, not the model's, so they are
    /// worded for that; the rest as everywhere else.
    static func message(_ error: CoreError) -> String {
        switch error {
        case .Network(let detail):
            String(localized: "Could not download from GitHub: \(detail)")
        case .Http(let status, let detail):
            String(localized: "GitHub answered \(String(status)): \(detail)")
        default:
            error.message
        }
    }
}

/// One skill: where it came from (and updating from there), its files, its
/// switch, and deleting it.
struct SkillDetailView: View {
    let folder: String
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var skill: Skill?
    @State private var files: [String] = []
    @State private var updating = false
    @State private var error: CoreError?
    @State private var confirmingDelete = false
    @State private var viewing: URL?

    var body: some View {
        Form {
            if let skill {
                if !skill.description.isEmpty {
                    Section { Text(skill.description).textSelection(.enabled) }
                }
                Section {
                    if let source = skill.source {
                        Text(source).font(.footnote.monospaced()).textSelection(.enabled)
                        Button {
                            Task { await update() }
                        } label: {
                            HStack {
                                Text("Update from GitHub")
                                if updating { Spacer(); ProgressView() }
                            }
                        }
                        .disabled(updating)
                    } else {
                        Text("From a file, pasted, or written by the model.").foregroundStyle(.secondary)
                    }
                } header: {
                    Text("Source")
                } footer: {
                    if let error {
                        Text(SkillAddSheet.message(error)).foregroundStyle(.red)
                    } else if skill.source != nil {
                        Text("Updating replaces the folder, and any change made to it here.")
                    }
                }
                Section {
                    ForEach(files, id: \.self) { file in
                        Button { viewing = fileURL(file) } label: {
                            Label(file, systemImage: file == "SKILL.md" ? "doc.text" : "doc")
                                .font(.footnote.monospaced())
                        }
                        .tint(.primary)
                    }
                } header: {
                    Text(SkillsView.status(skill).components(separatedBy: " · ").first ?? "")
                } footer: {
                    Text(skill.path)
                }
                Section {
                    Toggle(String(localized: "On"), isOn: Binding(
                        get: { skill.enabled },
                        set: { on in Task { await setEnabled(on) } }))
                }
                Section {
                    Button(String(localized: "Delete Skill"), role: .destructive) { confirmingDelete = true }
                }
            }
        }
        .navigationTitle(skill?.name ?? folder)
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog(
            String(localized: "Delete the skill “\(skill?.name ?? folder)” and its folder?"),
            isPresented: $confirmingDelete, titleVisibility: .visible
        ) {
            Button(String(localized: "Delete"), role: .destructive) { Task { await remove() } }
        }
        .sheet(item: $viewing) { url in FileViewer(url: url) }
        .onAppear { load() }
    }

    private func fileURL(_ file: String) -> URL? {
        guard let ws = app.core?.workspaceDir() else { return nil }
        return URL(fileURLWithPath: ws).appendingPathComponent("skills").appendingPathComponent(folder).appendingPathComponent(file)
    }

    private func load() {
        skill = app.core?.skills().first { $0.folder == folder }
        files = (try? app.core?.skillFiles(folder: folder)) ?? []
    }

    private func update() async {
        updating = true
        defer { updating = false }
        do {
            _ = try await app.core?.updateSkill(folder: folder)
            error = nil
        } catch let e as CoreError {
            error = e
        } catch {}
        load()
    }

    private func setEnabled(_ on: Bool) async {
        do {
            try await app.core?.setSkillEnabled(folder: folder, enabled: on)
        } catch let e as CoreError {
            error = e
        } catch {}
        load()
    }

    private func remove() async {
        do {
            try await app.core?.removeSkill(folder: folder)
            dismiss()
        } catch let e as CoreError {
            error = e
        } catch {}
    }
}
