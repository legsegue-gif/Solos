import AVKit
import SwiftMath
import SwiftUI

/// A reply's Markdown, drawn block by block; images, video and audio alone
/// in their paragraph are shown in place. Links go through the environment's
/// `openURL`, where the chat opens workspace files (`FileLinks`).
struct MarkdownView: View {
    let text: String
    @Environment(AppCore.self) private var app

    var body: some View {
        let blocks = MarkdownModel.parse(text)
        BlocksView(blocks: blocks)
            .environment(\.messageImages, images(in: blocks))
    }

    /// The workspace pictures shown in this reply, in order, for the viewer.
    private func images(in blocks: [MDBlock]) -> [URL] {
        blocks.compactMap { block in
            guard case .media(let src, _) = block, MediaView.kind(of: src) == .image else { return nil }
            return app.localFile(src)
        }
    }
}

/// Opens workspace links (`solos://ws/…`) in a preview; other links go where
/// the system sends them.
struct FileLinks: ViewModifier {
    @Environment(AppCore.self) private var app
    @State private var previewing: URL?

    func body(content: Content) -> some View {
        content
            .environment(\.openURL, OpenURLAction { url in
guard let local = app.localFile(url.absoluteString) else { return .systemAction }
                previewing = local
                return .handled
            })
            .sheet(item: $previewing) { FilePreview(url: $0).ignoresSafeArea() }
    }
}

extension URL: @retroactive Identifiable {
    public var id: String { absoluteString }
}

private struct BlocksView: View {
    let blocks: [MDBlock]

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                BlockView(block: block)
            }
        }
    }
}

private struct BlockView: View {
    let block: MDBlock

    var body: some View {
        switch block {
        case .heading(let level, let inline):
            InlineText(runs: inline, font: Self.headingFont(level))
                .padding(.top, level <= 2 ? 6 : 2)
        case .paragraph(let inline):
            InlineText(runs: inline, font: .body)
        case .code(let language, let code):
            CodeBlockView(language: language, code: code)
        case .math(let tex):
            // Centred when it fits, scrollable when it does not.
            ViewThatFits(in: .horizontal) {
                MathView(tex: tex, display: true).frame(maxWidth: .infinity)
                ScrollView(.horizontal, showsIndicators: false) { MathView(tex: tex, display: true) }
            }
            .padding(.vertical, 4)
        case .quote(let blocks):
            HStack(alignment: .top, spacing: 10) {
                RoundedRectangle(cornerRadius: 1.5).fill(Color.secondary.opacity(0.4)).frame(width: 3)
                BlocksView(blocks: blocks).foregroundStyle(.secondary)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .list(let ordered, let start, let items):
            ListView(ordered: ordered, start: start, items: items)
        case .table(let header, let rows, let alignments):
            TableView(header: header, rows: rows, alignments: alignments)
        case .rule:
            Divider().padding(.vertical, 4)
        case .media(let url, let alt):
            MediaView(source: url, alt: alt)
        }
    }

    static func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: .title2.bold()
        case 2: .title3.bold()
        case 3: .headline
        default: .subheadline.weight(.semibold)
        }
    }
}

/// Styled text with inline math drawn as images between the words.
private struct InlineText: View {
    let runs: MDInline
    let font: Font
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        text.font(font).fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
    }

    private var text: Text {
        // Text joined from pieces does not open its links, so text without
        // math stays a single attributed Text.
        if runs.allSatisfy({ if case .text = $0 { true } else { false } }) {
            let joined = runs.reduce(into: AttributedString()) { acc, run in
                if case .text(let a) = run { acc += a }
            }
            return Text(Self.styled(joined))
        }
        return runs.reduce(Text("")) { text, run in
            switch run {
            case .text(let a):
                return Text("\(text)\(Text(Self.styled(a)))")
            case .math(let tex):
                if let (image, descent) = MathView.render(tex, display: false, size: UIFont.preferredFont(forTextStyle: .body).pointSize, scheme: scheme) {
                    // Sit the formula's baseline on the text's.
                    return Text("\(text)\(Text(Image(uiImage: image)).baselineOffset(-descent))")
                }
                return Text("\(text)\(Text(tex).font(.body.monospaced()))")
            }
        }
    }

    /// Code spans on a tinted background, as in the reference app.
    static func styled(_ a: AttributedString) -> AttributedString {
        var a = a
        for run in a.runs where run.inlinePresentationIntent?.contains(.code) == true {
            a[run.range].backgroundColor = Color(.tertiarySystemFill)
            a[run.range].foregroundColor = Color.orange
        }
        return a
    }
}

/// TeX typeset by SwiftMath.
struct MathView: View {
    let tex: String
    let display: Bool
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        if let (image, _) = Self.render(tex, display: display, size: UIFont.preferredFont(forTextStyle: .body).pointSize * 1.1, scheme: scheme) {
            Image(uiImage: image)
        } else {
            // TeX SwiftMath cannot typeset: show the source, not nothing.
            Text(tex).font(.callout.monospaced()).foregroundStyle(.secondary)
        }
    }

    /// The typeset image and how far it reaches below the baseline.
    static func render(_ tex: String, display: Bool, size: CGFloat, scheme: ColorScheme) -> (UIImage, CGFloat)? {
        let color: UIColor = scheme == .dark ? .white : .black
        var renderer = SwiftMath.MathImage(latex: tex, fontSize: size, textColor: color,
                                           labelMode: display ? .display : .text)
        let (error, image, layout) = renderer.asImage()
        guard error == nil, let image else { return nil }
        return (image, layout?.descent ?? 0)
    }
}

private struct ListView: View {
    let ordered: Bool
    let start: Int
    let items: [MDListItem]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(items.enumerated()), id: \.offset) { i, item in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    marker(item, index: start + i)
                    BlocksView(blocks: item.blocks)
                }
            }
        }
    }

    @ViewBuilder
    private func marker(_ item: MDListItem, index: Int) -> some View {
        if let checked = item.checked {
            Image(systemName: checked ? "checkmark.square.fill" : "square")
                .foregroundStyle(checked ? Color.accentColor : .secondary)
        } else if ordered {
            Text("\(index).").monospacedDigit().foregroundStyle(.secondary)
        } else {
            Text("•").foregroundStyle(.secondary)
        }
    }
}

private struct TableView: View {
    let header: [MDInline]
    let rows: [[MDInline]]
    let alignments: [MDAlignment]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                GridRow {
                    ForEach(header.indices, id: \.self) { c in
                        cell(header[c], column: c).font(.subheadline.weight(.semibold))
                    }
                }
                .background(Color(.secondarySystemBackground))
                ForEach(rows.indices, id: \.self) { r in
                    Divider()
                    GridRow {
                        ForEach(header.indices, id: \.self) { c in
                            cell(c < rows[r].count ? rows[r][c] : [], column: c).font(.subheadline)
                        }
                    }
                }
            }
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color(.separator)))
            .clipShape(RoundedRectangle(cornerRadius: 8))
        }
    }

    private func cell(_ runs: MDInline, column: Int) -> some View {
        let alignment: Alignment = switch column < alignments.count ? alignments[column] : .leading {
        case .leading: .leading
        case .center: .center
        case .trailing: .trailing
        }
        return InlineText(runs: runs, font: .subheadline)
            .frame(minWidth: 60, maxWidth: 280, alignment: alignment)
            .padding(.horizontal, 10).padding(.vertical, 7)
    }
}

private struct CodeBlockView: View {
    let language: String?
    let code: String
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(language ?? "").font(.caption.monospaced()).foregroundStyle(.secondary)
                Spacer()
                Button {
                    UIPasteboard.general.string = code
                    copied = true
                    Task {
                        try? await Task.sleep(for: .seconds(1.5))
                        copied = false
                    }
                } label: {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc").font(.caption)
                }
                .accessibilityLabel(String(localized: "Copy code"))
            }
            .padding(.horizontal, 12).padding(.top, 8).padding(.bottom, 4)
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.system(.footnote, design: .monospaced))
                    .textSelection(.enabled)
                    .fixedSize(horizontal: true, vertical: true)
                    .padding(.horizontal, 12).padding(.bottom, 10)
            }
        }
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
    }
}

/// An image, video or audio file shown where the model put it.
struct MediaView: View {
    let source: String
    let alt: String
    @Environment(AppCore.self) private var app
    @Environment(\.messageImages) private var siblings
    @State private var viewing: URL?

    enum Kind { case image, video, audio, other }

    static func kind(of path: String) -> Kind {
        let ext = (path.split(separator: "?").first.map(String.init) ?? path).split(separator: ".").last?.lowercased() ?? ""
        switch ext {
        case "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "bmp", "tif", "tiff": return .image
        case "mp4", "mov", "m4v": return .video
        case "mp3", "m4a", "wav", "aac", "aiff", "caf", "flac": return .audio
        default: return .other
        }
    }

    var body: some View {
        let local = app.localFile(source)
        switch (Self.kind(of: source), local) {
        case (.image, .some(let url)):
            LocalImage(url: url, alt: alt)
                .onTapGesture { viewing = url }
                .accessibilityAddTraits(.isButton)
                .fullScreenCover(item: $viewing) { url in
                    let all = siblings.contains(url) ? siblings : [url]
                    ImageViewer(images: all, start: all.firstIndex(of: url) ?? 0)
                }
        case (.image, nil):
            if let remote = URL(string: source), remote.scheme?.hasPrefix("http") == true {
                AsyncImage(url: remote) { phase in
                    switch phase {
                    case .success(let image): image.resizable().scaledToFit()
                    case .failure: missing
                    default: ProgressView().frame(height: 120)
                    }
                }
                .frame(maxHeight: 360)
                .clipShape(RoundedRectangle(cornerRadius: 10))
            } else {
                missing
            }
        case (.video, .some(let url)):
            VideoPlayer(player: AVPlayer(url: url))
                .aspectRatio(16 / 9, contentMode: .fit)
                .clipShape(RoundedRectangle(cornerRadius: 10))
        case (.audio, .some(let url)):
            AudioRow(url: url, title: alt.isEmpty ? url.lastPathComponent : alt)
        default:
            Link(alt.isEmpty ? source : alt, destination: MarkdownModel.linkURL(source) ?? URL(string: "about:blank")!)
        }
    }

    private var missing: some View {
        Label(alt.isEmpty ? source : alt, systemImage: "photo.badge.exclamationmark")
            .font(.callout).foregroundStyle(.secondary)
    }
}

private struct LocalImage: View {
    let url: URL
    let alt: String
    @State private var image: UIImage?
    @State private var failed = false

    var body: some View {
        Group {
            if let image {
                // Never larger than the picture itself: a small icon stays small.
                Image(uiImage: image).resizable().scaledToFit()
                    .frame(maxWidth: image.size.width, maxHeight: min(image.size.height, 360))
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .accessibilityLabel(alt)
            } else if failed {
                Label(alt.isEmpty ? url.lastPathComponent : alt, systemImage: "photo.badge.exclamationmark")
                    .font(.callout).foregroundStyle(.secondary)
            } else {
                ProgressView().frame(height: 120)
            }
        }
        .task(id: url) {
            let loaded = await Task.detached(priority: .userInitiated) { UIImage(contentsOfFile: url.path) }.value
            image = loaded
            failed = loaded == nil
        }
    }
}

private struct AudioRow: View {
    let url: URL
    let title: String
    @State private var player: AVPlayer?
    @State private var playing = false

    var body: some View {
        Button {
            if player == nil { player = AVPlayer(url: url) }
            if playing { player?.pause() } else { player?.play() }
            playing.toggle()
        } label: {
            HStack(spacing: 10) {
                Image(systemName: playing ? "pause.circle.fill" : "play.circle.fill").font(.title2)
                Text(title).lineLimit(1)
                Spacer()
            }
            .padding(10)
            .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        }
        .buttonStyle(.plain)
        .onReceive(NotificationCenter.default.publisher(for: AVPlayerItem.didPlayToEndTimeNotification)) { note in
            if (note.object as? AVPlayerItem) == player?.currentItem {
                playing = false
                player?.seek(to: .zero)
            }
        }
    }
}
