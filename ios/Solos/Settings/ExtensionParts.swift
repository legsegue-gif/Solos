import SwiftUI
import UniformTypeIdentifiers

/// What skills and MCP servers share in Settings: one kind of row, one way
/// of choosing where a new one comes from, one editor for named values.

/// A skill or server in its list: its name, a line saying what it is, a
/// line of state (red when it went wrong), and its switch.
struct ExtensionRow<Detail: View>: View {
    let title: String
    let subtitle: String
    let status: String
    var failed = false
    @Binding var enabled: Bool
    @ViewBuilder let detail: () -> Detail

    var body: some View {
        HStack {
            NavigationLink(destination: detail) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).foregroundStyle(enabled ? Color.primary : Color.secondary)
                    if !subtitle.isEmpty {
                        Text(subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                    }
                    Text(status).font(.caption).foregroundStyle(failed ? Color.red : Color.secondary).lineLimit(2)
                }
            }
            Toggle(String(localized: "On"), isOn: $enabled)
                .labelsHidden()
                .fixedSize()
        }
    }
}

/// Where a new skill or server comes from.
enum AddSource: String, CaseIterable, Identifiable {
    case link, paste, file
    var id: Self { self }

    var label: String {
        switch self {
        case .link: String(localized: "Link")
        case .paste: String(localized: "Paste")
        case .file: String(localized: "File")
        }
    }
}

struct AddSourcePicker: View {
    @Binding var source: AddSource

    var body: some View {
        Picker(String(localized: "Source"), selection: $source) {
            ForEach(AddSource.allCases) { Text($0.label).tag($0) }
        }
        .pickerStyle(.segmented)
        .listRowBackground(Color.clear)
        .listRowInsets(EdgeInsets())
    }
}

/// A text area for JSON or a SKILL.md: no capitals, no corrections, and
/// with `straightQuotes` the URL keyboard, whose quotes stay straight (the
/// default one makes them curly, which is not JSON).
struct PasteEditor: View {
    @Binding var text: String
    var straightQuotes = false

    var body: some View {
        TextEditor(text: $text)
            .font(.system(.footnote, design: .monospaced))
            .frame(minHeight: 200)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .keyboardType(straightQuotes ? .URL : .default)
    }
}

/// A file the user picked, copied where the app may read it after the
/// picker's access ends.
enum PickedFile {
    static func copy(_ url: URL) throws -> URL {
        let access = url.startAccessingSecurityScopedResource()
        defer { if access { url.stopAccessingSecurityScopedResource() } }
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let dest = dir.appendingPathComponent(url.lastPathComponent)
        try FileManager.default.copyItem(at: url, to: dest)
        return dest
    }
}

/// Named values (an MCP server's headers or environment), each value hidden
/// until shown, since they are often keys. Hidden values are dots, not a
/// secure field: any secure field makes iOS treat the page as a login and
/// offer Passwords on every field of it.
struct KeyValueEditor: View {
    @Binding var pairs: [KeyValue]
    let keyPlaceholder: String

    var body: some View {
        ForEach($pairs) { $pair in
            VStack(alignment: .leading, spacing: 6) {
                TextField(keyPlaceholder, text: $pair.key)
                    .font(.body.monospaced())
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                HStack {
                    Group {
                        if pair.shown {
                            TextField(String(localized: "Value"), text: $pair.value)
                                .keyboardType(.URL)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                        } else {
                            Text(String(repeating: "•", count: min(max(pair.value.count, 1), 16)))
                                .foregroundStyle(.secondary)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                    .font(.footnote.monospaced())
                    Button { pair.shown.toggle() } label: {
                        Image(systemName: pair.shown ? "eye.slash" : "eye")
                    }
                    .buttonStyle(.borderless)
                    .accessibilityLabel(pair.shown ? String(localized: "Hide value") : String(localized: "Show value"))
                }
            }
        }
        .onDelete { pairs.remove(atOffsets: $0) }
        // A new row is shown, to be typed into.
        Button(String(localized: "Add")) { pairs.append(KeyValue(key: "", value: "", shown: true)) }
    }
}

struct KeyValue: Identifiable, Equatable {
    let id = UUID()
    var key: String
    var value: String
    var shown = false

    static func from(_ map: [String: String]) -> [KeyValue] {
        map.keys.sorted().map { KeyValue(key: $0, value: map[$0] ?? "") }
    }

    /// Rows with no name are dropped.
    static func map(_ pairs: [KeyValue]) -> [String: String] {
        Dictionary(pairs.compactMap { p in
            let k = p.key.trimmingCharacters(in: .whitespaces)
            return k.isEmpty ? nil : (k, p.value)
        }, uniquingKeysWith: { _, last in last })
    }

    static func == (a: KeyValue, b: KeyValue) -> Bool { a.key == b.key && a.value == b.value }
}
