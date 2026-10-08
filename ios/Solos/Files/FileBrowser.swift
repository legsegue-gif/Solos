import QuickLook
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// The Linux the model and the terminal use, as folders: opens at the
/// workspace, and the path bar walks up to `/`.
///
/// Files are read straight from the device; nothing goes through the
/// emulator. Only the workspace may be changed here (import, delete, copy,
/// move): everything else is the guest's own filesystem, which keeps its
/// metadata in a database of its own, and a file changed behind its back
/// would not match it. For the same reason a guest symlink (`/bin/sh`)
/// shows as the small file that stores its target.
struct FileBrowser: View {
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var path: String = "/solos/ws"
    @AppStorage("files.sort") private var sort: FileSort = .name
    @AppStorage("files.ascending") private var ascending = true
    @AppStorage("files.foldersFirst") private var foldersFirst = true
    @AppStorage("files.showHidden") private var showHidden = false
    @State private var entries: [FileEntry] = []
    @State private var previewing: URL?
    @State private var importing = false
    @State private var deleting: FileEntry?
    @State private var transferring: (entry: FileEntry, move: Bool)?
    @State private var problem: String?

    var body: some View {
        NavigationStack {
            List {
                ForEach(entries) { entry in
                    row(entry)
                }
                if entries.isEmpty {
                    Text("This folder is empty.").foregroundStyle(.secondary)
                }
            }
            .listStyle(.plain)
            .safeAreaInset(edge: .top, spacing: 0) { pathBar }
            .navigationTitle(String(localized: "Files"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(String(localized: "Close")) { dismiss() }
                }
                ToolbarItem(placement: .primaryAction) { menu }
            }
            .refreshable { load() }
            .onAppear(perform: load)
            .onChange(of: path) { load() }
            .onChange(of: sort) { load() }
            .onChange(of: ascending) { load() }
            .onChange(of: foldersFirst) { load() }
            .onChange(of: showHidden) { load() }
            .sheet(item: $previewing) { FileViewer(url: $0).ignoresSafeArea() }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
                if case .success(let urls) = result { importFiles(urls) }
            }
            .confirmationDialog(
                String(localized: "Delete \(deleting?.name ?? "")?"),
                isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
                titleVisibility: .visible
            ) {
                Button(String(localized: "Delete"), role: .destructive) {
                    if let d = deleting { remove(d) }
                }
            }
            .sheet(isPresented: Binding(get: { transferring != nil }, set: { if !$0 { transferring = nil } })) {
                if let t = transferring {
                    FolderPicker(start: path, title: t.move ? String(localized: "Move to") : String(localized: "Copy to")) { destination in
                        transfer(t.entry, to: destination, move: t.move)
                    }
                }
            }
            .alert(problem ?? "", isPresented: Binding(get: { problem != nil }, set: { if !$0 { problem = nil } })) {
                Button(String(localized: "OK")) {}
            }
        }
    }

    // MARK: Pieces

    /// No scroll view here: against the navigation bar, a horizontal one got
    /// the bar's height as a top inset from the second opening of the sheet
    /// on, and drew the crumbs out of sight (seen on screen). A path too long
    /// for the width keeps its end, where the reader is.
    private var pathBar: some View {
        ViewThatFits(in: .horizontal) {
            crumbs.frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
            crumbs.frame(minWidth: 0, maxWidth: .infinity, alignment: .trailing).clipped()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(.bar)
    }

    private var crumbs: some View {
        HStack(spacing: 4) {
            ForEach(Array(GuestPath.crumbs(path).enumerated()), id: \.offset) { index, crumb in
                if index > 0 { Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.secondary) }
                Button(crumb.name) { path = crumb.path }
                    .font(.callout)
                    .lineLimit(1)
                    .disabled(crumb.path == path)
            }
        }
        .fixedSize()
    }

    private var menu: some View {
        Menu {
            Button { path = GuestPath.parent(path) } label: {
                Label(String(localized: "Go to parent folder"), systemImage: "arrow.up.doc")
            }
            .disabled(path == "/")
            Divider()
            Button { importing = true } label: {
                Label(String(localized: "Import file"), systemImage: "plus")
            }
            .disabled(!GuestPath.isWritable(path, mounts: mounts))
            Divider()
            Picker(String(localized: "Sort by"), selection: $sort) {
                ForEach(FileSort.allCases, id: \.self) { Text($0.label).tag($0) }
            }
            Toggle(isOn: $ascending) { Label(String(localized: "Ascending"), systemImage: "arrow.up") }
            Toggle(isOn: $foldersFirst) { Label(String(localized: "Folders first"), systemImage: "folder") }
            Toggle(isOn: $showHidden) { Label(String(localized: "Show hidden files"), systemImage: "eye") }
        } label: {
            Label(String(localized: "More"), systemImage: "ellipsis")
        }
    }

    @ViewBuilder
    private func row(_ entry: FileEntry) -> some View {
        let writable = GuestPath.isWritable(entry.guestPath, mounts: mounts)
        Button {
            if entry.isDirectory { path = entry.guestPath } else { previewing = entry.url }
        } label: {
            HStack(spacing: 12) {
                Image(systemName: entry.isDirectory ? "folder.fill" : FileEntry.icon(entry.name))
                    .font(.title3)
                    .foregroundStyle(entry.isDirectory ? Color.accentColor : .secondary)
                    .frame(width: 28)
                VStack(alignment: .leading, spacing: 2) {
                    Text(entry.name).foregroundStyle(.primary).lineLimit(1).truncationMode(.middle)
                    Text(entry.subtitle).font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                if entry.isDirectory { Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary) }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .contextMenu {
            Button { UIPasteboard.general.string = entry.guestPath } label: {
                Label(String(localized: "Copy path"), systemImage: "doc.on.doc")
            }
            if !entry.isDirectory {
                ShareLink(item: entry.url) { Label(String(localized: "Share"), systemImage: "square.and.arrow.up") }
            }
            Button { transferring = (entry, false) } label: {
                Label(String(localized: "Copy to…"), systemImage: "doc.on.doc.fill")
            }
            Button { transferring = (entry, true) } label: {
                Label(String(localized: "Move to…"), systemImage: "folder")
            }
            .disabled(!writable)
            Divider()
            Button(role: .destructive) { deleting = entry } label: {
                Label(String(localized: "Delete"), systemImage: "trash")
            }
            .disabled(!writable)
        }
    }

    // MARK: Actions

    private var guestRoot: URL? {
        app.core?.guestRootDir().map { URL(fileURLWithPath: $0) }
    }

    /// The folders shared with the model, which show under /solos/mnt.
    private var mounts: [Mount] { app.core?.mounts() ?? [] }

    private func device(_ guest: String) -> URL? {
        guard let root = guestRoot else { return nil }
        return GuestPath.device(guest, root: root, workspace: app.core.map { URL(fileURLWithPath: $0.workspaceDir()) }, mounts: mounts)
    }

    private func load() {
        let shared = mounts
        // /solos/mnt is not in the Linux system: it lists the shared folders,
        // and /solos shows it beside ws.
        if path == GuestPath.mountsRoot {
            entries = shared.map {
                FileEntry(url: URL(fileURLWithPath: $0.path), name: $0.name, guestPath: GuestPath.child(path, $0.name),
                          isDirectory: true, size: 0, modified: nil)
            }
            .sorted(by: FileSort.order(sort, ascending: ascending, foldersFirst: foldersFirst))
            return
        }
        guard let dir = device(path) else {
            entries = []
            return
        }
        var found = FileEntry.list(dir, guestPath: path, showHidden: showHidden)
        if path == "/solos", !shared.isEmpty, !found.contains(where: { $0.name == "mnt" }) {
            found.append(FileEntry(url: dir, name: "mnt", guestPath: GuestPath.mountsRoot, isDirectory: true, size: 0, modified: nil))
        }
        entries = found.sorted(by: FileSort.order(sort, ascending: ascending, foldersFirst: foldersFirst))
    }

    private func importFiles(_ urls: [URL]) {
        guard let dir = device(path) else { return }
        for source in urls {
            let scoped = source.startAccessingSecurityScopedResource()
            defer { if scoped { source.stopAccessingSecurityScopedResource() } }
            do {
                try FileManager.default.copyItem(at: source, to: FileEntry.free(dir.appendingPathComponent(source.lastPathComponent)))
            } catch {
                problem = error.localizedDescription
            }
        }
        load()
    }

    private func remove(_ entry: FileEntry) {
        do {
            try FileManager.default.removeItem(at: entry.url)
        } catch {
            problem = error.localizedDescription
        }
        deleting = nil
        load()
    }

    private func transfer(_ entry: FileEntry, to guestFolder: String, move: Bool) {
        guard GuestPath.isWritable(guestFolder, mounts: mounts), let dir = device(guestFolder) else {
            problem = String(localized: "Files can only be put in the workspace (/solos/ws) or a shared folder that allows changes.")
            return
        }
        let target = FileEntry.free(dir.appendingPathComponent(entry.name))
        do {
            if move {
                try FileManager.default.moveItem(at: entry.url, to: target)
            } else {
                try FileManager.default.copyItem(at: entry.url, to: target)
            }
        } catch {
            problem = error.localizedDescription
        }
        transferring = nil
        load()
    }
}

/// Paths as the guest names them.
enum GuestPath {
    static let workspace = "/solos/ws"
    /// Where the folders the user shares with the model show.
    static let mountsRoot = "/solos/mnt"

    /// The shared folder a path is in, and the rest of the path inside it.
    static func mount(_ path: String, in mounts: [Mount]) -> (mount: Mount, rest: String)? {
        guard path.hasPrefix(mountsRoot + "/") else { return nil }
        let after = path.dropFirst(mountsRoot.count + 1)
        let name = String(after.prefix(while: { $0 != "/" }))
        guard let found = mounts.first(where: { $0.name == name }) else { return nil }
        return (found, String(after.dropFirst(name.count)).trimmingCharacters(in: CharacterSet(charactersIn: "/")))
    }

    /// Inside the workspace, or in a shared folder the user allowed changes
    /// in: the places the app may change.
    static func isWritable(_ path: String, mounts: [Mount] = []) -> Bool {
        if path == workspace || path.hasPrefix(workspace + "/") { return true }
        return mount(path, in: mounts)?.mount.writable ?? false
    }

    static func parent(_ path: String) -> String {
        guard path != "/" else { return "/" }
        let parent = (path as NSString).deletingLastPathComponent
        return parent.isEmpty ? "/" : parent
    }

    static func child(_ path: String, _ name: String) -> String {
        path == "/" ? "/\(name)" : "\(path)/\(name)"
    }

    /// `/`, then each folder on the way down, for the path bar.
    static func crumbs(_ path: String) -> [(name: String, path: String)] {
        var out: [(String, String)] = [("/", "/")]
        var so = ""
        for part in path.split(separator: "/") {
            so += "/\(part)"
            out.append((String(part), so))
        }
        return out
    }

    /// The device folder for a guest path. The workspace is its own folder
    /// on the device (the guest sees it through a mount), so it is mapped
    /// directly rather than through the guest's tree.
    static func device(_ guest: String, root: URL, workspace: URL?, mounts: [Mount] = []) -> URL {
        if let (shared, rest) = mount(guest, in: mounts) {
            let base = URL(fileURLWithPath: shared.path)
            return rest.isEmpty ? base : base.appendingPathComponent(rest)
        }
        if let workspace, guest == self.workspace || guest.hasPrefix(self.workspace + "/") {
            let rest = guest.dropFirst(self.workspace.count).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            return rest.isEmpty ? workspace : workspace.appendingPathComponent(rest)
        }
        let rest = guest.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        return rest.isEmpty ? root : root.appendingPathComponent(rest)
    }
}

enum FileSort: String, CaseIterable {
    case name, modified, size, kind

    var label: String {
        switch self {
        case .name: String(localized: "Name")
        case .modified: String(localized: "Date modified")
        case .size: String(localized: "Size")
        case .kind: String(localized: "Kind")
        }
    }

    static func order(_ sort: FileSort, ascending: Bool, foldersFirst: Bool) -> (FileEntry, FileEntry) -> Bool {
        { a, b in
            if foldersFirst, a.isDirectory != b.isDirectory { return a.isDirectory }
            let byName = a.name.localizedStandardCompare(b.name)
            let result: ComparisonResult = switch sort {
            case .name: byName
            case .modified: a.modified == b.modified ? byName : (a.modified ?? .distantPast) < (b.modified ?? .distantPast) ? .orderedAscending : .orderedDescending
            case .size: a.size == b.size ? byName : a.size < b.size ? .orderedAscending : .orderedDescending
            case .kind:
                a.kind == b.kind ? byName : a.kind.localizedStandardCompare(b.kind)
            }
            return ascending ? result == .orderedAscending : result == .orderedDescending
        }
    }
}

struct FileEntry: Identifiable {
    let url: URL
    let name: String
    let guestPath: String
    let isDirectory: Bool
    let size: Int64
    let modified: Date?
    var id: String { guestPath }
    var kind: String { isDirectory ? "" : (name as NSString).pathExtension.lowercased() }

    var subtitle: String {
        let date = modified?.formatted(date: .abbreviated, time: .shortened) ?? ""
        if isDirectory { return date }
        let size = ByteCountFormatter.string(fromByteCount: size, countStyle: .file)
        return date.isEmpty ? size : "\(date) · \(size)"
    }

    static func list(_ dir: URL, guestPath: String, showHidden: Bool) -> [FileEntry] {
        let keys: [URLResourceKey] = [.isDirectoryKey, .fileSizeKey, .contentModificationDateKey]
        // A mount is a symlink on the device: read what it points at.
        let found = (try? FileManager.default.contentsOfDirectory(
            at: dir.resolvingSymlinksInPath(), includingPropertiesForKeys: keys, options: [])) ?? []
        return found.compactMap { url in
            let name = url.lastPathComponent
            if !showHidden, name.hasPrefix(".") { return nil }
            let target = url.resolvingSymlinksInPath()
            let values = try? target.resourceValues(forKeys: Set(keys))
            return FileEntry(
                url: target, name: name, guestPath: GuestPath.child(guestPath, name),
                isDirectory: values?.isDirectory ?? false,
                size: Int64(values?.fileSize ?? 0), modified: values?.contentModificationDate)
        }
    }

    /// `name`, or `name 2.ext` and so on when taken.
    static func free(_ url: URL) -> URL {
        guard FileManager.default.fileExists(atPath: url.path) else { return url }
        let dir = url.deletingLastPathComponent()
        let ext = url.pathExtension
        let stem = url.deletingPathExtension().lastPathComponent
        var n = 2
        while true {
            let candidate = dir.appendingPathComponent(ext.isEmpty ? "\(stem) \(n)" : "\(stem) \(n).\(ext)")
            if !FileManager.default.fileExists(atPath: candidate.path) { return candidate }
            n += 1
        }
    }

    static func icon(_ name: String) -> String {
        switch (name as NSString).pathExtension.lowercased() {
        case "png", "jpg", "jpeg", "gif", "heic", "webp", "svg": "photo"
        case "mp4", "mov", "m4v": "film"
        case "mp3", "m4a", "wav", "aac": "waveform"
        case "pdf": "doc.richtext"
        case "zip", "gz", "tgz", "tar", "xz", "apk": "shippingbox"
        case "sh", "py", "js", "ts", "rs", "swift", "c", "h", "go", "rb": "chevron.left.forwardslash.chevron.right"
        case "md", "txt", "log", "json", "csv", "yaml", "yml", "conf", "toml": "doc.text"
        default: "doc"
        }
    }
}

/// Choose a folder in the workspace.
private struct FolderPicker: View {
    let start: String
    let title: String
    let chosen: (String) -> Void
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var path: String = GuestPath.workspace

    var body: some View {
        NavigationStack {
            List {
                if path != GuestPath.workspace {
                    Button { path = GuestPath.parent(path) } label: {
                        Label("..", systemImage: "arrow.up")
                    }
                }
                ForEach(folders, id: \.self) { name in
                    Button { path = GuestPath.child(path, name) } label: {
                        Label(name, systemImage: "folder")
                    }
                }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .safeAreaInset(edge: .top, spacing: 0) {
                Text(path).font(.caption.monospaced()).frame(maxWidth: .infinity).padding(8).background(.bar)
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button(String(localized: "Cancel")) { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button(String(localized: "Choose")) {
                        chosen(path)
                        dismiss()
                    }
                }
            }
            .onAppear { if GuestPath.isWritable(start) { path = start } }
        }
    }

    private var folders: [String] {
        guard let ws = app.core.map({ URL(fileURLWithPath: $0.workspaceDir()) }) else { return [] }
        let dir = GuestPath.device(path, root: ws, workspace: ws)
        return FileEntry.list(dir, guestPath: path, showHidden: false).filter(\.isDirectory).map(\.name)
            .sorted { $0.localizedStandardCompare($1) == .orderedAscending }
    }
}

/// QuickLook for what it can show; plain text for the rest (Linux files
/// often have no extension to tell QuickLook they are text).
struct FileViewer: View {
    let url: URL

    var body: some View {
        if QLPreviewController.canPreview(url as NSURL), !url.pathExtension.isEmpty {
            FilePreview(url: url)
        } else {
            TextFileView(url: url)
        }
    }
}

private struct TextFileView: View {
    let url: URL
    @Environment(\.dismiss) private var dismiss
    /// Enough to read, small enough not to stall the view.
    private static let limit = 400_000

    var body: some View {
        NavigationStack {
            ScrollView([.vertical]) {
                Text(content)
                    .font(.system(.footnote, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()
            }
            .navigationTitle(url.lastPathComponent)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button(String(localized: "Close")) { dismiss() } }
                ToolbarItem(placement: .primaryAction) { ShareLink(item: url) }
            }
        }
    }

    private var content: String {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return String(localized: "This file cannot be read.") }
        defer { try? handle.close() }
        let data = (try? handle.read(upToCount: Self.limit)) ?? Data()
        guard let text = String(data: data, encoding: .utf8) else {
            return String(localized: "This file is not text, and the system has no preview for it. Share it to open it in another app.")
        }
        let size = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? Int) ?? data.count
        return size > data.count
            ? text + "\n\n" + String(localized: "[showing the first \(String(data.count / 1024)) KB of \(String(size / 1024)) KB]")
            : text
    }
}
