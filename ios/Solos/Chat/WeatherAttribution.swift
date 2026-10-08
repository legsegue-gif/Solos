import SwiftUI
import WeatherKit

/// Apple's required credit for weather data: the Apple Weather mark and a link
/// to the legal page, both supplied by WeatherKit (`attribution`). Shown with
/// weather the assistant fetched (the tool's detail), and in Settings. When
/// WeatherKit cannot be asked (no network, the service unavailable), the words
/// and the link to Apple's page stand in for the mark.
struct WeatherAttribution: View {
    @Environment(\.colorScheme) private var scheme
    @State private var attribution: WeatherAttribution_?

    /// The part of WeatherKit's answer this needs, kept apart from SwiftUI's
    /// own types.
    struct WeatherAttribution_ {
        let mark: URL
        let legal: URL
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let mark = attribution?.mark {
                AsyncImage(url: mark) { image in
                    image.resizable().scaledToFit()
                } placeholder: {
                    Text(verbatim: "\u{F8FF} Weather").font(.footnote.weight(.semibold))
                }
                .frame(height: 16, alignment: .leading)
            } else {
                Text(verbatim: "\u{F8FF} Weather").font(.footnote.weight(.semibold))
            }
            Link(String(localized: "Other data sources"), destination: attribution?.legal ?? Self.fallbackLegal)
                .font(.caption)
        }
        .task(id: scheme) { await load() }
        .accessibilityElement(children: .combine)
    }

    private static let fallbackLegal = URL(string: "https://weatherkit.apple.com/legal-attribution.html")!

    private func load() async {
        guard let found = try? await WeatherService.shared.attribution else { return }
        attribution = WeatherAttribution_(mark: scheme == .dark ? found.combinedMarkDarkURL : found.combinedMarkLightURL, legal: found.legalPageURL)
    }
}
