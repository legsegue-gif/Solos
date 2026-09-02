import SwiftUI

// MARK: - View Model

class StorageManagementViewModel: ObservableObject {
    struct SessionStorage: Identifiable {
        let id: String
        let title: String?
        var solosSize: Int64 = 0
        var totalSize: Int64 { solosSize }
    }

    @Published var shellContainerSize: Int64 = 0
    @Published var chatDatabaseSize: Int64 = 0
    @Published var sessions: [SessionStorage] = []
    @Published var isLoading = true

    private let fm = FileManager.default
    private let formatter: ByteCountFormatter = {
        let f = ByteCountFormatter()
        f.allowedUnits = [.useBytes, .useKB, .useMB, .useGB]
        f.countStyle = .file
        return f
    }()

    func format(_ bytes: Int64) -> String {
        formatter.string(fromByteCount: bytes)
    }

    var totalSessionSize: Int64 {
        sessions.reduce(0) { $0 + $1.totalSize }
    }

    func load() {
        isLoading = true
        Task.detached(priority: .userInitiated) { [weak self] in
            guard let self else { return }

            let libraryURL = self.fm.urls(for: .libraryDirectory, in: .userDomainMask).first!
            let documentsURL = self.fm.urls(for: .documentDirectory, in: .userDomainMask).first!

            // Shell container
            let rootfsURL = documentsURL.appendingPathComponent("alpine-rootfs", isDirectory: true)
            let shellSize = self.directorySize(at: rootfsURL)

            // Chat database
            let dbURL = libraryURL.appendingPathComponent("SolosChat/solos.db")
            let dbSize = self.fileSize(at: dbURL)

            // Per-session solos files: Library/SolosChat/solos/<sessionId>/
            let solosBaseURL = libraryURL.appendingPathComponent("SolosChat/solos", isDirectory: true)
            // Get all chat sessions
            let chatSessions = await ChatStore.shared.listSessions()

            // Build a set of session IDs
            let sessionIds = Set(chatSessions.map(\.id))

            // Enumerate solos directories
            var solosSizes: [String: Int64] = [:]
            if let contents = try? self.fm.contentsOfDirectory(at: solosBaseURL, includingPropertiesForKeys: nil, options: .skipsHiddenFiles) {
                for dir in contents {
                    let sid = dir.lastPathComponent
                    if sessionIds.contains(sid) {
                        solosSizes[sid] = self.directorySize(at: dir)
                    }
                }
            }

            // Build session storage entries
            let sorted: [SessionStorage] = chatSessions.map { session in
                SessionStorage(
                    id: session.id,
                    title: session.title,
                    solosSize: solosSizes[session.id] ?? 0
                )
            }.sorted { $0.totalSize > $1.totalSize }

            await MainActor.run {
                self.shellContainerSize = shellSize
                self.chatDatabaseSize = dbSize
                self.sessions = sorted
                self.isLoading = false
            }
        }
    }

    private func directorySize(at url: URL) -> Int64 {
        guard let enumerator = fm.enumerator(
            at: url,
            includingPropertiesForKeys: [.fileSizeKey, .isDirectoryKey],
            options: [.skipsHiddenFiles]
        ) else { return 0 }

        var total: Int64 = 0
        for case let fileURL as URL in enumerator {
            let values = try? fileURL.resourceValues(forKeys: [.fileSizeKey, .isDirectoryKey])
            if values?.isDirectory == false {
                total += Int64(values?.fileSize ?? 0)
            }
        }
        return total
    }

    private func fileSize(at url: URL) -> Int64 {
        let values = try? url.resourceValues(forKeys: [.fileSizeKey])
        return Int64(values?.fileSize ?? 0)
    }
}

// MARK: - Storage Management View

struct StorageManagementView: View {
    @StateObject private var vm = StorageManagementViewModel()

    var body: some View {
        List {
            Section("Overview") {
                storageRow(icon: "terminal", color: .gray, label: "Shell Container", value: vm.format(vm.shellContainerSize))
                storageRow(icon: "cylinder", color: .blue, label: "Chat Database", value: vm.format(vm.chatDatabaseSize))
                storageRow(icon: "doc", color: .indigo, label: "Session Files", value: vm.format(vm.totalSessionSize))
            }

            Section("Sessions") {
                if vm.isLoading {
                    HStack {
                        Spacer()
                        ProgressView()
                        Spacer()
                    }
                } else if vm.sessions.isEmpty {
                    Text("No sessions")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(vm.sessions) { session in
                        NavigationLink {
                            SessionStorageDetailView(session: session, onFilesCleared: { vm.load() })
                        } label: {
                            HStack {
                                Text(session.title ?? "Untitled")
                                    .lineLimit(1)
                                Spacer()
                                Text(vm.format(session.totalSize))
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
        }
        .navigationTitle("Storage")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { vm.load() }
    }

    private func storageRow(icon: String, color: Color, label: String, value: String) -> some View {
        HStack {
            Image(systemName: icon)
                .font(.system(size: 9))
                .foregroundStyle(.white)
                .frame(width: 21, height: 21)
                .background(color, in: Circle())
            Text(label)
            Spacer()
            Text(value)
                .foregroundStyle(.secondary)
        }
    }
}

// MARK: - Session Storage Detail View

struct SessionStorageDetailView: View {
    let session: StorageManagementViewModel.SessionStorage
    var onFilesCleared: (() -> Void)?

    @State private var showClearConfirmation = false
    @State private var isClearing = false
    @State private var currentSolosSize: Int64

    init(session: StorageManagementViewModel.SessionStorage, onFilesCleared: (() -> Void)? = nil) {
        self.session = session
        self.onFilesCleared = onFilesCleared
        _currentSolosSize = State(initialValue: session.solosSize)
    }

    private let formatter: ByteCountFormatter = {
        let f = ByteCountFormatter()
        f.allowedUnits = [.useBytes, .useKB, .useMB, .useGB]
        f.countStyle = .file
        return f
    }()

    private var solosURL: URL {
        let lib = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask).first!
        return lib.appendingPathComponent("SolosChat/solos/\(session.id)", isDirectory: true)
    }

    private var totalFileSize: Int64 { currentSolosSize }
    private var hasFiles: Bool { totalFileSize > 0 }

    var body: some View {
        List {
            Section("Solos Files") {
                if currentSolosSize > 0 {
                    NavigationLink {
                        FileBrowserView(rootPath: solosURL)
                    } label: {
                        HStack {
                            Label("Browse Files", systemImage: "folder")
                            Spacer()
                            Text(formatter.string(fromByteCount: currentSolosSize))
                                .foregroundStyle(.secondary)
                        }
                    }
                } else {
                    Text("No solos files")
                        .foregroundStyle(.secondary)
                }
            }

            Section {
                Button(role: .destructive) {
                    showClearConfirmation = true
                } label: {
                    HStack {
                        if isClearing {
                            ProgressView()
                                .controlSize(.small)
                            Text("Clearing…")
                        } else {
                            Label("Clear Session Files", systemImage: "trash")
                        }
                        Spacer()
                        if !isClearing {
                            Text(formatter.string(fromByteCount: totalFileSize))
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .disabled(!hasFiles || isClearing)
            } footer: {
                Text("Removes all files generated by this session. The conversation itself will be preserved.")
            }
        }
        .navigationTitle(session.title ?? "Session")
        .navigationBarTitleDisplayMode(.inline)
        .alert("Clear Session Files?", isPresented: $showClearConfirmation) {
            Button("Clear \(formatter.string(fromByteCount: totalFileSize))", role: .destructive) {
                clearFiles()
            }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("This will delete \(formatter.string(fromByteCount: totalFileSize)) of files. This action cannot be undone.")
        }
    }

    private func clearFiles() {
        isClearing = true
        let sessionId = session.id
        Task.detached(priority: .userInitiated) {
            let fm = FileManager.default
            let lib = fm.urls(for: .libraryDirectory, in: .userDomainMask).first!
            let solosDir = lib.appendingPathComponent("SolosChat/solos/\(sessionId)", isDirectory: true)
            try? fm.removeItem(at: solosDir)

            await MainActor.run {
                currentSolosSize = 0
                isClearing = false
                onFilesCleared?()
            }
        }
    }
}
