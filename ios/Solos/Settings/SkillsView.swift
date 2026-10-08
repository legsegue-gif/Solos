import SwiftUI

/// The installed skills: each can be read, turned off or deleted, and new
/// ones come from a GitHub link. The model installs them the same way when
/// asked in a chat; both end up as folders in the workspace's `skills`.
struct SkillsView: View {
    @Environment(AppCore.self) private var app
    @State private var skills: [Skill] = []
    @State private var adding = false
    @State private var link = ""
    @State private var installing = false
    @State private var error: CoreError?
    @State private var deleting: Skill?

    var body: some View {
        Form {
            Section {
                ForEach(skills, id: \.folder) { skill in
                    row(skill)
                }
                .onDelete { offsets in deleting = offsets.first.map { skills[$0] } }
                if skills.isEmpty && !installing {
                    Text("No skills yet.").foregroundStyle(.secondary)
                }
                if installing {
                    HStack {
                        Text("Installing…").foregroundStyle(.secondary)
                        Spacer()
                        ProgressView()
                    }
                }
            } footer: {
                if let error {
                    Text(Self.message(error)).foregroundStyle(.red)
                } else {
                    Text("A skill is a folder of instructions, and often scripts, that the model reads when a task calls for it. You can also ask in a chat: “install this skill: <link>”. Skills are kept in the workspace's skills folder.")
                }
            }
            // A repository with several skills: offer each, as the error
            // above names them.
            if case .NoSingleSkill(_, let candidates)? = error, !candidates.isEmpty {
                Section {
                    ForEach(candidates, id: \.self) { candidate in
                        Button(candidate.split(separator: "/").last.map(String.init) ?? candidate) {
                            Task { await install(candidate) }
                        }
                        .disabled(installing)
                    }
                } header: {
                    Text("Skills in this repository")
                }
            }
        }
        .navigationTitle(String(localized: "Skills"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button { link = ""; adding = true } label: { Image(systemName: "plus") }
                    .accessibilityLabel(String(localized: "Add skill"))
                    .disabled(installing)
            }
        }
        .alert(String(localized: "Add a skill from GitHub"), isPresented: $adding) {
            TextField("https://github.com/owner/repo", text: $link)
                .keyboardType(.URL)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button(String(localized: "Cancel"), role: .cancel) {}
            Button(String(localized: "Add")) { Task { await install(link) } }
        } message: {
            Text("A link to a repository, or to the folder in one that holds the SKILL.md.")
        }
        .confirmationDialog(
            String(localized: "Delete the skill “\(deleting?.name ?? "")” and its folder?"),
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible
        ) {
            Button(String(localized: "Delete"), role: .destructive) {
                if let skill = deleting { Task { await remove(skill) } }
            }
        }
        .onAppear { load() }
    }

    private func row(_ skill: Skill) -> some View {
        HStack {
            NavigationLink { SkillDetailView(skill: skill) } label: {
                VStack(alignment: .leading, spacing: 2) {
                    Text(skill.name).foregroundStyle(skill.enabled ? Color.primary : Color.secondary)
                    if !skill.description.isEmpty {
                        Text(skill.description).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                    }
                }
            }
            Toggle(String(localized: "On"), isOn: Binding(
                get: { skill.enabled },
                set: { on in Task { await setEnabled(skill, on) } }))
                .labelsHidden()
                .fixedSize()
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

    private func load() {
        skills = app.core?.skills() ?? []
    }

    private func install(_ source: String) async {
        let source = source.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !source.isEmpty else { return }
        installing = true
        defer { installing = false; load() }
        do {
            _ = try await app.core?.installSkill(source: source)
            error = nil
        } catch let e as CoreError {
            error = e
        } catch {}
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

    private func remove(_ skill: Skill) async {
        deleting = nil
        do {
            try await app.core?.removeSkill(folder: skill.folder)
            error = nil
        } catch let e as CoreError {
            error = e
        } catch {}
        load()
    }
}

/// A skill's `SKILL.md`, as the model reads it, and where it is; it can be
/// deleted from here too, since a swipe on the list is easy to miss.
struct SkillDetailView: View {
    let skill: Skill
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var error: CoreError?
    @State private var confirmingDelete = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text(skill.path + "/SKILL.md")
                    .font(.caption).foregroundStyle(.secondary)
                    .textSelection(.enabled)
                if let error {
                    Text(error.message).foregroundStyle(.red)
                } else {
                    Text(text)
                        .font(.system(.footnote, design: .monospaced))
                        .textSelection(.enabled)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
        }
        .navigationTitle(skill.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button(role: .destructive) { confirmingDelete = true } label: { Image(systemName: "trash") }
                    .accessibilityLabel(String(localized: "Delete"))
            }
        }
        .confirmationDialog(
            String(localized: "Delete the skill “\(skill.name)” and its folder?"),
            isPresented: $confirmingDelete, titleVisibility: .visible
        ) {
            Button(String(localized: "Delete"), role: .destructive) { Task { await remove() } }
        }
        .onAppear {
            do {
                text = try app.core?.skillInstructions(folder: skill.folder) ?? ""
                error = nil
            } catch let e as CoreError {
                error = e
            } catch {}
        }
    }

    private func remove() async {
        do {
            try await app.core?.removeSkill(folder: skill.folder)
            dismiss()
        } catch let e as CoreError {
            error = e
        } catch {}
    }
}
