import PhotosUI
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// A file picked for the next message, held in the app's temporary folder
/// until the core has copied it into the workspace.
struct PendingAttachment: Identifiable, Equatable {
    let id = UUID()
    let name: String
    let url: URL
    let mime: String
    var isImage: Bool { mime.hasPrefix("image/") }

    var source: AttachmentSource {
        AttachmentSource(name: name, hostPath: url.path, mime: mime)
    }

    static func mime(for url: URL) -> String {
        UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
    }

    /// A fresh folder per attachment, so two files with one name never
    /// collide before the core renames them.
    private static func tempURL(named name: String) throws -> URL {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("attachments", isDirectory: true)
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent(name)
    }

    /// A photo from the library as JPEG with its long edge at most 2000
    /// pixels, as the reference app sends it: HEIC is not read by most
    /// models, and a 48-megapixel original is mostly wasted tokens.
    static func photo(_ item: PhotosPickerItem, index: Int) async throws -> PendingAttachment? {
        guard let data = try await item.loadTransferable(type: Data.self) else { return nil }
        let jpeg = try await Task.detached(priority: .userInitiated) { () -> Data in
            guard let image = UIImage(data: data) else { throw CocoaError(.fileReadCorruptFile) }
            return resized(image, maxLongEdge: 2000).jpegData(compressionQuality: 0.85) ?? data
        }.value
        let stamp = Date().formatted(.verbatim("\(year: .defaultDigits)\(month: .twoDigits)\(day: .twoDigits)-\(hour: .twoDigits(clock: .twentyFourHour, hourCycle: .zeroBased))\(minute: .twoDigits)\(second: .twoDigits)",
                                                timeZone: .current, calendar: .current))
        let url = try tempURL(named: "photo-\(stamp)-\(index + 1).jpg")
        try jpeg.write(to: url)
        return PendingAttachment(name: url.lastPathComponent, url: url, mime: "image/jpeg")
    }

    /// A file from the Files app, copied out of its security scope.
    static func file(_ picked: URL) throws -> PendingAttachment {
        let scoped = picked.startAccessingSecurityScopedResource()
        defer { if scoped { picked.stopAccessingSecurityScopedResource() } }
        let url = try tempURL(named: picked.lastPathComponent)
        try FileManager.default.copyItem(at: picked, to: url)
        return PendingAttachment(name: picked.lastPathComponent, url: url, mime: mime(for: picked))
    }

    func discard() {
        try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
    }

    private static func resized(_ image: UIImage, maxLongEdge: CGFloat) -> UIImage {
        let size = image.size
        let long = max(size.width, size.height) * image.scale
        guard long > maxLongEdge else { return image }
        let factor = maxLongEdge / long
        let target = CGSize(width: size.width * image.scale * factor, height: size.height * image.scale * factor)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        return UIGraphicsImageRenderer(size: target, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: target))
        }
    }
}

/// The attachments waiting to be sent, above the text field.
struct PendingAttachmentsRow: View {
    let items: [PendingAttachment]
    let remove: (PendingAttachment) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(items) { item in
                    ZStack(alignment: .topTrailing) {
                        if item.isImage, let image = UIImage(contentsOfFile: item.url.path) {
                            Image(uiImage: image).resizable().scaledToFill()
                                .frame(width: 56, height: 56)
                                .clipShape(RoundedRectangle(cornerRadius: 8))
                        } else {
                            FileChip(name: item.name).frame(height: 56)
                        }
                        Button { remove(item) } label: {
                            Image(systemName: "xmark.circle.fill")
                                .symbolRenderingMode(.palette)
                                .foregroundStyle(.white, .black.opacity(0.6))
                        }
                        .offset(x: 6, y: -6)
                        .accessibilityLabel(String(localized: "Remove \(item.name)"))
                    }
                }
            }
            .padding(.top, 6).padding(.trailing, 6)
        }
    }
}

struct FileChip: View {
    let name: String

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "doc").foregroundStyle(.secondary)
            Text(name).font(.caption).lineLimit(1).truncationMode(.middle)
        }
        .padding(.horizontal, 10).padding(.vertical, 8)
        .frame(maxWidth: 200)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 8))
    }
}

/// What a sent user message carried: images as thumbnails, other files as
/// chips; either opens in the previewer.
struct SentAttachments: View {
    let parts: [Part]
    @Environment(AppCore.self) private var app
    @Environment(\.openURL) private var openURL

    var body: some View {
        let items: [(path: String, name: String, image: Bool)] = parts.compactMap { part in
            if case .attachment(let path, let name, _, _) = part { return (path, name, part.isImage) }
            return nil
        }
        if !items.isEmpty {
            HStack(spacing: 6) {
                ForEach(items, id: \.path) { item in
                    Button { open(item.path) } label: {
                        if item.image, let url = app.localFile(item.path), let image = UIImage(contentsOfFile: url.path) {
                            Image(uiImage: image).resizable().scaledToFill()
                                .frame(width: 96, height: 96)
                                .clipShape(RoundedRectangle(cornerRadius: 10))
                        } else {
                            FileChip(name: item.name)
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(item.name)
                }
            }
        }
    }

    private func open(_ guestPath: String) {
        let url = guestPath.replacingOccurrences(of: "/solos/ws/", with: "solos://ws/")
        if let u = MarkdownModel.linkURL(url) { openURL(u) }
    }
}

extension Part {
    var isImage: Bool {
        if case .attachment(_, _, let mime, _) = self { mime.hasPrefix("image/") } else { false }
    }
}
