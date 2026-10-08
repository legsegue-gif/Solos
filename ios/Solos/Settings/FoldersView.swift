import SwiftUI
import UniformTypeIdentifiers

/// Settings ▸ Files ▸ Shared folders: folders of the device (iCloud Drive, On My
/// iPhone, another app's) the model may read, and change if the user allows.
struct FoldersView: View {
    @Environment(AppCore.self) private var app
    @State private var picking = false
    @State private var problem: String?

    var body: some View {
        let store = app.mountStore
        Form {
            Section {
                ForEach(store.entries) { entry in
                    NavigationLink { FolderDetail(id: entry.id) } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(entry.name)
                            Text(subtitle(entry, store)).font(.caption).foregroundStyle(store.isAvailable(entry) ? Color.secondary : Color.red)
                        }
                    }
                }
                .onDelete { offsets in
                    for i in offsets { try? store.remove(store.entries[i].id) }
                }
                Button(String(localized: "Add folder")) { picking = true }
            } footer: {
                if let problem {
                    Text(problem).foregroundStyle(.red)
                } else {
                    Text("The model can read a shared folder, and change it only if you allow that. The terminal and shell cannot see these folders; the model copies files between them and the workspace.")
                }
            }
        }
        .navigationTitle(String(localized: "Shared folders"))
        .navigationBarTitleDisplayMode(.inline)
        .fileImporter(isPresented: $picking, allowedContentTypes: [.folder]) { result in
            switch result {
            case .success(let url):
                do { try store.add(url); problem = nil } catch { problem = error.localizedDescription }
            case .failure(let error):
                problem = error.localizedDescription
            }
        }
    }

    private func subtitle(_ entry: MountStore.Entry, _ store: MountStore) -> String {
        guard store.isAvailable(entry) else { return String(localized: "Not available. Remove it and choose the folder again.") }
        let access = entry.writable && store.systemWritable(entry) ? String(localized: "Changes allowed") : String(localized: "Read-only")
        return "/solos/mnt/\(entry.name) · \(access)"
    }
}

private struct FolderDetail: View {
    let id: UUID
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var problem: String?
    @State private var confirmingRemove = false

    var body: some View {
        let store = app.mountStore
        Form {
            if let entry = store.entries.first(where: { $0.id == id }) {
                Section {
                    TextField(String(localized: "Name"), text: $name)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .onSubmit { commitName(store) }
                } header: {
                    Text("Name")
                } footer: {
                    if let problem { Text(problem).foregroundStyle(.red) }
                    else { Text("The model sees this folder as /solos/mnt/\(name).") }
                }
                Section {
                    Toggle(String(localized: "Allow changes"), isOn: Binding(
                        get: { entry.writable },
                        set: { try? store.setWritable(id, $0) }))
                        .disabled(!store.systemWritable(entry))
                } footer: {
                    if store.systemWritable(entry) {
                        Text("Off: the model can read and copy files out of this folder. On: it can also write, edit and copy files into it.")
                    } else {
                        Text("The system does not let Solos change this folder, so it stays read-only.")
                    }
                }
                if let path = store.displayPath(entry) {
                    Section {
                        LabeledContent(String(localized: "Folder"), value: entry.folderName)
                        Text(path).font(.caption.monospaced()).foregroundStyle(.secondary).textSelection(.enabled)
                    }
                }
                Section {
                    Button(String(localized: "Stop sharing"), role: .destructive) { confirmingRemove = true }
                }
            }
        }
        .navigationTitle(name)
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { name = store.entries.first(where: { $0.id == id })?.name ?? "" }
        .onDisappear { commitName(store) }
        .confirmationDialog(String(localized: "Stop sharing this folder?"), isPresented: $confirmingRemove, titleVisibility: .visible) {
            Button(String(localized: "Stop sharing"), role: .destructive) {
                try? store.remove(id)
                dismiss()
            }
        } message: {
            Text("Nothing in the folder is deleted.")
        }
    }

    private func commitName(_ store: MountStore) {
        guard let current = store.entries.first(where: { $0.id == id })?.name, name != current else { return }
        if let p = store.problem(with: name, for: id) {
            problem = p
            return
        }
        problem = nil
        try? store.rename(id, to: name)
    }
}
