import SwiftUI

/// Where one package manager (`apk`, `pip` or `npm`) gets packages from:
/// its sources, the one in use checked. A speed test times them all at
/// once, from this device, and switches to the fastest (the official one
/// when it is); any row can also be chosen by hand.
struct MirrorsView: View {
    let kind: MirrorKind
    @Environment(AppCore.self) private var app
    @State private var mirrors: [PackageMirror] = []
    @State private var chosen = ""
    @State private var speeds: [String: UInt64?] = [:]
    @State private var testing = false
    @State private var busy = false
    @State private var error: CoreError?

    var body: some View {
        Form {
            Section {
                ForEach(sorted, id: \.id) { mirror in
                    row(mirror)
                }
            } footer: {
                Text(Self.note(kind))
            }
            Section {
                Button { Task { await testAndChoose() } } label: {
                    HStack {
                        Text("Test speed and use the fastest")
                        if testing { Spacer(); ProgressView() }
                    }
                }
                .disabled(testing)
            } footer: {
                if let error {
                    Text(error.message).foregroundStyle(.red)
                } else {
                    Text("Times every source from this device and switches to the fastest. A newly installed Linux system does this on its own.")
                }
            }
        }
        .navigationTitle(Self.title(kind))
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { load() }
    }

    /// The reference app's order until a test has run; then fastest first,
    /// the ones that failed last, as the reference app sorts its results.
    private var sorted: [PackageMirror] {
        guard !speeds.isEmpty else { return mirrors }
        return mirrors.enumerated().sorted { a, b in
            switch (speeds[a.element.id] ?? nil, speeds[b.element.id] ?? nil) {
            case let (x?, y?): x < y
            case (.some, .none): true
            case (.none, .some): false
            case (.none, .none): a.offset < b.offset
            }
        }.map(\.element)
    }

    /// Name (the official source's in bold) and region, as the reference app
    /// shows them, and the address in grey under them.
    private func row(_ mirror: PackageMirror) -> some View {
        Button { Task { await choose(mirror) } } label: {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(mirror.name)
                            .fontWeight(mirror.id == "official" ? .semibold : .regular)
                            .foregroundStyle(Color.primary)
                        Text(Self.region(mirror.region))
                            .font(.caption2).foregroundStyle(Color.secondary)
                    }
                    Text(mirror.url)
                        .font(.caption).foregroundStyle(Color.secondary).lineLimit(1)
                }
                Spacer()
                if let speed = speeds[mirror.id] {
                    Text(speed.map { "\($0) ms" } ?? String(localized: "Failed"))
                        .font(.callout).monospacedDigit()
                        .foregroundStyle(speed == nil ? Color.red : Color.secondary)
                }
                if chosen == mirror.id {
                    Image(systemName: "checkmark").foregroundStyle(.tint)
                }
            }
        }
        .disabled(busy)
    }

    static func title(_ kind: MirrorKind) -> String {
        switch kind {
        case .alpine: "Alpine APK"
        case .pip: "Python pip"
        case .npm: "Node.js npm"
        }
    }

    private static func note(_ kind: MirrorKind) -> String {
        switch kind {
        case .alpine: String(localized: "Where apk installs Linux packages from.")
        case .pip: String(localized: "For pip, once Python is installed (apk add py3-pip).")
        case .npm: String(localized: "For npm, once Node.js is installed (apk add npm).")
        }
    }

    /// Where a mirror is, in the person's language.
    static func region(_ region: String) -> String {
        switch region {
        case "Global": String(localized: "Global")
        case "China": String(localized: "China")
        case "Europe": String(localized: "Europe")
        case "Asia": String(localized: "Asia")
        default: region
        }
    }

    private func load() {
        mirrors = (app.core?.packageMirrors() ?? []).filter { $0.kind == kind }
        chosen = app.core?.chosenPackageMirrors().first { $0.kind == kind }?.id ?? ""
    }

    private func choose(_ mirror: PackageMirror) async {
        busy = true
        defer { busy = false }
        do {
            try await app.core?.setPackageMirror(kind: mirror.kind, id: mirror.id)
            chosen = mirror.id
            error = nil
        } catch let e as CoreError {
            error = e
        } catch {}
    }

    /// Times this kind's sources and switches to the fastest that answered,
    /// official included; nothing changes when none did.
    private func testAndChoose() async {
        testing = true
        defer { testing = false }
        do {
            let results = try await app.core?.testPackageMirrors(kind: kind) ?? []
            speeds = Dictionary(uniqueKeysWithValues: results.map { ($0.id, $0.millis) })
            error = nil
            let fastest = results.compactMap { r in r.millis.map { (r.id, $0) } }.min { $0.1 < $1.1 }?.0
            if let fastest, fastest != chosen, let mirror = mirrors.first(where: { $0.id == fastest }) {
                await choose(mirror)
            }
        } catch let e as CoreError {
            error = e
        } catch {}
    }
}
