//  BrowserPrefs.swift — this phone's browser settings.
//
//  Split out of the settings view because the browser bridge reads these
//  before any view exists — and because the headless probe links the bridge
//  without linking the UI.

import Foundation
import WebKit

/// Persisted on the device: these describe this phone's browser, not the
/// conversation, and they have to be readable before the core is up.
enum BrowserPrefs {
    static let userAgentKey = "browser.userAgent"

    enum Agent: String, CaseIterable, Identifiable {
        // No "leave WebKit's own string alone": that string has no
        // `Safari/` token, and sites read it as an app's embedded view. X
        // answers it with a 302 to `x-safari-https://`, which a web view
        // cannot follow — every tweet was "Frame load interrupted"
        // (2026-09-24).
        case mobile, desktop

        var id: String { rawValue }

        var label: String {
            switch self {
            case .mobile: return String(localized: "Mobile")
            case .desktop: return String(localized: "Desktop")
            }
        }

        var note: String {
            switch self {
            case .mobile: return String(localized: "Says iPhone Safari outright; some sites accept nothing else.")
            case .desktop: return String(localized: "Asks as Mac Safari and gets the desktop page: more on it, and more to click.")
            }
        }

        var string: String {
            switch self {
            case .mobile:
                return "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) "
                    + "AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1"
            case .desktop:
                return "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
                    + "AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15"
            }
        }
    }

    static var agent: Agent {
        get {
            Agent(rawValue: UserDefaults.standard.string(forKey: userAgentKey) ?? "") ?? .mobile
        }
        set {
            UserDefaults.standard.set(newValue.rawValue, forKey: userAgentKey)
            apply(newValue)
        }
    }

    /// Applies to the tabs that are already open as well as the next one, so
    /// the change takes effect without making the user reopen anything.
    static func apply(_ agent: Agent = BrowserPrefs.agent) {
        for page in TabSet.shared.pages {
            page.web.customUserAgent = agent.string
        }
    }
}
