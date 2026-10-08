import Foundation
import Observation

/// The folders the user shares with the model. The system's permission to a
/// folder is a bookmark, kept here; the core is told each folder's name and
/// where it is, and checks every path the model writes.
@MainActor
@Observable
final class MountStore {
    struct Entry: Codable, Identifiable, Equatable {
        var id: UUID
        var name: String
        /// What the user allowed; a folder the system will not let the app
        /// change stays read-only whatever this says.
        var writable: Bool
        var bookmark: Data
        /// The folder's own name when it was chosen, for the list.
        var folderName: String
    }

    private(set) var entries: [Entry] = []
    /// Folders whose bookmark opened this launch, by entry; the app holds the
    /// permission to each for as long as it runs.
    private(set) var open: [UUID: URL] = [:]
    @ObservationIgnored private var core: SolosCore?
    @ObservationIgnored private let file: URL

    init(file: URL? = nil) {
        self.file = file ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Solos").appendingPathComponent("folders.json")
    }

    func start(core: SolosCore) {
        self.core = core
        entries = (try? JSONDecoder().decode([Entry].self, from: Data(contentsOf: file))) ?? []
        for entry in entries { reopen(entry) }
        push()
    }

    /// The folder can be opened again.
    func isAvailable(_ entry: Entry) -> Bool { open[entry.id] != nil }

    /// Whether the system lets this app change the folder.
    func systemWritable(_ entry: Entry) -> Bool {
        guard let url = open[entry.id] else { return false }
        return FileManager.default.isWritableFile(atPath: url.path)
    }

    func displayPath(_ entry: Entry) -> String? { open[entry.id]?.path }

    // MARK: Changes

    /// Share a folder the user picked. The name is the folder's, made usable
    /// and unique.
    @discardableResult
    func add(_ url: URL) throws -> Entry {
        let scoped = url.startAccessingSecurityScopedResource()
        let bookmark: Data
        do {
            bookmark = try url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil)
        } catch {
            if scoped { url.stopAccessingSecurityScopedResource() }
            throw error
        }
        let entry = Entry(
            id: UUID(), name: freeName(Self.usableName(url.lastPathComponent)), writable: false,
            bookmark: bookmark, folderName: url.lastPathComponent)
        entries.append(entry)
        if scoped { open[entry.id] = url } else { reopen(entry) }
        try save()
        push()
        return entry
    }

    func rename(_ id: UUID, to name: String) throws {
        guard let i = entries.firstIndex(where: { $0.id == id }) else { return }
        entries[i].name = name
        try save()
        push()
    }

    func setWritable(_ id: UUID, _ on: Bool) throws {
        guard let i = entries.firstIndex(where: { $0.id == id }) else { return }
        entries[i].writable = on
        try save()
        push()
    }

    func remove(_ id: UUID) throws {
        open[id]?.stopAccessingSecurityScopedResource()
        open[id] = nil
        entries.removeAll { $0.id == id }
        try save()
        push()
    }

    /// Why a name cannot be used, or `nil`.
    func problem(with name: String, for id: UUID?) -> String? {
        if !Self.isUsable(name) {
            return String(localized: "Use a name without slashes, colons or percent signs, not starting with a dot.")
        }
        if entries.contains(where: { $0.id != id && $0.name.lowercased() == name.lowercased() }) {
            return String(localized: "Another shared folder has this name.")
        }
        return nil
    }

    // MARK: Names

    nonisolated static func isUsable(_ name: String) -> Bool {
        !name.isEmpty && name.count <= 64 && !name.hasPrefix(".") && name == name.trimmingCharacters(in: .whitespaces)
            && !name.contains(where: { "/\\:%?#".contains($0) || $0.isNewline || $0.asciiValue.map { $0 < 32 } ?? false })
    }

    nonisolated static func usableName(_ raw: String) -> String {
        var name = String(raw.map { "/\\:%?#".contains($0) ? "-" : $0 }.filter { !$0.isNewline })
        name = name.trimmingCharacters(in: .whitespaces)
        while name.hasPrefix(".") { name.removeFirst() }
        name = String(name.prefix(64)).trimmingCharacters(in: .whitespaces)
        return name.isEmpty ? "Folder" : name
    }

    private func freeName(_ name: String) -> String {
        let taken = Set(entries.map { $0.name.lowercased() })
        guard taken.contains(name.lowercased()) else { return name }
        var n = 2
        while taken.contains("\(name) \(n)".lowercased()) { n += 1 }
        return "\(name) \(n)"
    }

    // MARK: Persistence and the core

    private func reopen(_ entry: Entry) {
        var stale = false
        guard let url = try? URL(resolvingBookmarkData: entry.bookmark, options: [], relativeTo: nil, bookmarkDataIsStale: &stale),
              url.startAccessingSecurityScopedResource() else { return }
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory), isDirectory.boolValue else {
            url.stopAccessingSecurityScopedResource()
            return
        }
        open[entry.id] = url
        // The system asks for a fresh bookmark when the folder has moved.
        if stale, let fresh = try? url.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil),
           let i = entries.firstIndex(where: { $0.id == entry.id }) {
            entries[i].bookmark = fresh
            try? save()
        }
    }

    private func save() throws {
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(entries).write(to: file, options: .atomic)
    }

    /// Tell the core which folders there are now: those that opened, with
    /// changes allowed only where the user and the system both allow them.
    private func push() {
        let mounts = entries.compactMap { entry -> Mount? in
            guard let url = open[entry.id] else { return nil }
            return Mount(name: entry.name, path: url.path, writable: entry.writable && systemWritable(entry))
        }
        do {
            try core?.setMounts(mounts: mounts)
        } catch {
            NSLog("setMounts: \(error)")
        }
    }
}
