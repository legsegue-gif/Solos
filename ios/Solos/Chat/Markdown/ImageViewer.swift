import Photos
import SwiftUI
import UIKit

/// The pictures of one message, for the viewer to page through.
struct MessageImages: EnvironmentKey {
    static let defaultValue: [URL] = []
}

extension EnvironmentValues {
    var messageImages: [URL] {
        get { self[MessageImages.self] }
        set { self[MessageImages.self] = newValue }
    }
}

/// Full screen, one picture per page, swiping between the pictures of the
/// same message; pinch or double-tap to zoom. Saving has its own button —
/// it is what people do most with a picture, and inside the share sheet it
/// is two steps away.
struct ImageViewer: View {
    let images: [URL]
    @Environment(\.dismiss) private var dismiss
    @State private var index: Int
    @State private var sharing: URL?
    @State private var saveState: SaveState = .idle

    enum SaveState: Equatable {
        case idle, saving, saved
        case failed(String)
    }

    init(images: [URL], start: Int) {
        self.images = images
        _index = State(initialValue: start)
    }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            TabView(selection: $index) {
                ForEach(Array(images.enumerated()), id: \.offset) { i, url in
                    ZoomableImage(url: url).tag(i)
                }
            }
            .tabViewStyle(.page(indexDisplayMode: images.count > 1 ? .automatic : .never))
            .ignoresSafeArea()

            VStack {
                HStack {
                    circleButton("xmark", label: String(localized: "Close")) { dismiss() }
                    Spacer()
                    circleButton(saveGlyph, label: String(localized: "Save to Photos")) { save() }
                        .disabled(saveState == .saving)
                    circleButton("square.and.arrow.up", label: String(localized: "Share")) { sharing = images[index] }
                }
                .padding(.horizontal, 16)
                Spacer()
                // "Did it save?" is the one question the button settles, so
                // a refusal is said, not swallowed.
                if case .failed(let reason) = saveState {
                    Text(reason)
                        .font(.footnote).foregroundStyle(.white).multilineTextAlignment(.center)
                        .padding(.horizontal, 14).padding(.vertical, 8)
                        .background(Color.red.opacity(0.75), in: Capsule())
                        .padding(.bottom, 28)
                }
            }
        }
        .sheet(item: $sharing) { ShareSheet(items: [$0]) }
        .onChange(of: index) { saveState = .idle }
    }

    private var saveGlyph: String {
        switch saveState {
        case .saved: "checkmark"
        default: "square.and.arrow.down"
        }
    }

    private func circleButton(_ glyph: String, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Group {
                if glyph == "square.and.arrow.down" && saveState == .saving {
                    ProgressView().tint(.white)
                } else {
                    Image(systemName: glyph)
                }
            }
            .font(.system(size: 15, weight: .semibold))
            .foregroundStyle(.white)
            .frame(width: 36, height: 36)
            .background(Color.white.opacity(0.18), in: Circle())
        }
        .accessibilityLabel(label)
    }

    /// Add-only access: permission to put a picture in, not to read the
    /// library. From the file, so the album gets the original bytes.
    private func save() {
        let url = images[index]
        saveState = .saving
        PHPhotoLibrary.requestAuthorization(for: .addOnly) { status in
            guard status == .authorized || status == .limited else {
                finish(.failed(String(localized: "Solos may not add to your photos. You can allow it in Settings.")))
                return
            }
            PHPhotoLibrary.shared().performChanges {
                PHAssetCreationRequest.forAsset().addResource(with: .photo, fileURL: url, options: nil)
            } completionHandler: { ok, error in
                finish(ok ? .saved : .failed(error?.localizedDescription ?? String(localized: "Could not save the picture.")))
            }
        }
    }

    private nonisolated func finish(_ state: SaveState) {
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                withAnimation { saveState = state }
                let linger: Double = state == .saved ? 2 : 4
                DispatchQueue.main.asyncAfter(deadline: .now() + linger) {
                    MainActor.assumeIsolated {
                        if saveState == state { withAnimation { saveState = .idle } }
                    }
                }
            }
        }
    }
}

/// Pinch to zoom, double-tap to fill, drag while zoomed. The drag is only
/// active when zoomed in: installed at 1× it takes the swipe that turns the
/// page.
private struct ZoomableImage: View {
    let url: URL
    @State private var scale: CGFloat = 1
    @State private var committed: CGFloat = 1
    @State private var offset: CGSize = .zero
    @State private var committedOffset: CGSize = .zero

    var body: some View {
        GeometryReader { geo in
            if let image = UIImage(contentsOfFile: url.path) {
                Image(uiImage: image)
                    .resizable().scaledToFit()
                    .frame(width: geo.size.width, height: geo.size.height)
                    .scaleEffect(scale)
                    .offset(offset)
                    .gesture(
                        MagnifyGesture()
                            .onChanged { scale = max(1, committed * $0.magnification) }
                            .onEnded { _ in
                                committed = scale
                                if scale <= 1 { reset() }
                            }
                    )
                    .simultaneousGesture(
                        DragGesture()
                            .onChanged { g in
                                offset = CGSize(width: committedOffset.width + g.translation.width,
                                                height: committedOffset.height + g.translation.height)
                            }
                            .onEnded { _ in committedOffset = offset },
                        including: scale > 1 ? .all : .subviews
                    )
                    .onTapGesture(count: 2) {
                        withAnimation(.easeOut(duration: 0.2)) {
                            if scale > 1 { reset() } else { scale = 2.5; committed = 2.5 }
                        }
                    }
                    .accessibilityLabel(url.lastPathComponent)
            } else {
                Text("This picture cannot be opened.")
                    .foregroundStyle(.white.opacity(0.7))
                    .frame(width: geo.size.width, height: geo.size.height)
            }
        }
    }

    private func reset() {
        scale = 1
        committed = 1
        offset = .zero
        committedOffset = .zero
    }
}

struct ShareSheet: UIViewControllerRepresentable {
    let items: [Any]
    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }
    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}
