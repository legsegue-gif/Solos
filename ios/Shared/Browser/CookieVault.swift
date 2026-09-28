//  CookieVault.swift — keeping a login that the system keeps throwing away.
//
//  The whole premise of the browser here is "log in once by hand, then let the
//  model carry on in the same session". WebKit's tracking prevention does not
//  know that: it classifies busy domains as prevalent and wipes all of their
//  site data — HttpOnly login cookies included — after a stretch without a
//  real user gesture. Agent-driven loads are never a gesture, so exactly the
//  logins this feature exists for are the ones that expire.
//
//  So: before the browser is used, snapshot every cookie, and put back the
//  ones that have gone missing. The live store always wins; this only ever
//  restores what is absent.
//
//  Where it is kept matters. Not the Keychain, and not anywhere that syncs:
//  these are live credentials for one device and they should not travel. A
//  single file in Application Support, protected until first unlock, excluded
//  from backup, and outside the guest rootfs so shell code cannot read it.
//
//  It is bounded by use, not by time alone: a domain nobody has visited in a
//  month stops being restored, so the file tracks what you actually use.

import Foundation
import WebKit

@MainActor
final class CookieVault {
    static let shared = CookieVault()

    /// The first use after launch always syncs — that is the one that
    /// recovers from a wipe that happened while the app was not running.
    private static let interval: TimeInterval = 60
    /// Mirrors ITP's own window, but any real navigation counts, not just a
    /// gesture.
    private static let retention: TimeInterval = 30 * 24 * 3600
    private static let maxSites = 500

    private var lastSync: Date?
    private var syncing = false
    private var visits: [String: Date] = [:]
    private var loaded = false

    private struct Stored: Codable {
        var cookies: [Item] = []
        var visits: [String: Date] = [:]
    }

    private struct Item: Codable {
        var name: String
        var value: String
        var domain: String
        var path: String
        /// Unix seconds. Nil is a session cookie, which is worth keeping:
        /// plenty of sites sign you in with one.
        var expires: Double?
        var secure: Bool
        var httpOnly: Bool

        var key: String { "\(domain)|\(path)|\(name)" }
        var site: String { CookieVault.site(of: domain) }

        var isExpired: Bool {
            guard let expires else { return false }
            return Date(timeIntervalSince1970: expires) < Date()
        }

        init(_ cookie: HTTPCookie) {
            name = cookie.name
            value = cookie.value
            domain = cookie.domain
            path = cookie.path
            expires = cookie.expiresDate?.timeIntervalSince1970
            secure = cookie.isSecure
            httpOnly = cookie.isHTTPOnly
        }

        var cookie: HTTPCookie? {
            var properties: [HTTPCookiePropertyKey: Any] = [
                .name: name,
                .value: value,
                .domain: domain,
                .path: path,
            ]
            if secure { properties[.secure] = "TRUE" }
            if let expires { properties[.expires] = Date(timeIntervalSince1970: expires) }
            // Not in the public enum, but WebKit reads it, and a login cookie
            // restored without its HttpOnly flag is a different cookie.
            if httpOnly { properties[HTTPCookiePropertyKey("HttpOnly")] = "TRUE" }
            return HTTPCookie(properties: properties)
        }
    }

    // MARK: - Use

    /// Note that a page was actually loaded for this URL. Aging is keyed on
    /// navigations rather than on the restores themselves, so a dead domain
    /// cannot keep itself alive by being restored forever.
    func noteVisit(_ url: URL?) {
        guard let host = url?.host(), !host.isEmpty else { return }
        load()
        visits[Self.site(of: host)] = Date()
    }

    /// Snapshot, then put back whatever the system has taken. Throttled, and
    /// safe to call before every browser operation.
    func syncIfNeeded() {
        guard !syncing else { return }
        if let lastSync, Date().timeIntervalSince(lastSync) < Self.interval { return }
        syncing = true
        lastSync = Date()
        Task { await sync() }
    }

    private func sync() async {
        defer { syncing = false }
        load()
        let store = WKWebsiteDataStore.default().httpCookieStore
        let live = await store.allCookies()
        let liveKeys = Set(live.map { Item($0).key })

        var backed = Dictionary(uniqueKeysWithValues: loadItems().map { ($0.key, $0) })
        // The live store is the truth for anything it still has.
        for cookie in live {
            let item = Item(cookie)
            backed[item.key] = item
        }

        // Anything the backup has and the live store does not is what a wipe
        // took. This is the whole point of the file.
        var restored = 0
        for (key, item) in backed where !liveKeys.contains(key) {
            guard !item.isExpired, Self.isFresh(visits[item.site]) else { continue }
            if let cookie = item.cookie {
                await store.setCookie(cookie)
                restored += 1
            }
        }

        backed = backed.filter { !$0.value.isExpired && Self.isFresh(visits[$0.value.site]) }
        prune(&backed)
        save(Array(backed.values))
        if restored > 0 {
            NSLog("[CookieVault] restored \(restored) cookie(s) the system had dropped")
        }
    }

    private static func isFresh(_ visit: Date?) -> Bool {
        guard let visit else { return false }
        return Date().timeIntervalSince(visit) < retention
    }

    /// Keep the file from growing without bound on a device nobody tidies.
    private func prune(_ items: inout [String: Item]) {
        visits = visits.filter { Self.isFresh($0.value) }
        guard visits.count > Self.maxSites else { return }
        let keep = Set(
            visits.sorted { $0.value > $1.value }
                .prefix(Self.maxSites)
                .map(\.key)
        )
        visits = visits.filter { keep.contains($0.key) }
        items = items.filter { keep.contains($0.value.site) }
    }

    /// "news.ycombinator.com" and "ycombinator.com" are the same login as far
    /// as ageing is concerned. Not a public-suffix list — this only decides
    /// what to keep, and being one label too coarse costs nothing.
    nonisolated private static func site(of host: String) -> String {
        let bare = host.hasPrefix(".") ? String(host.dropFirst()) : host
        let parts = bare.split(separator: ".")
        guard parts.count > 2 else { return bare.lowercased() }
        return parts.suffix(2).joined(separator: ".").lowercased()
    }

    // MARK: - The file

    private static let fileName = "browser-cookies.json"

    private var fileURL: URL? {
        guard let base = try? FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true
        ) else { return nil }
        return base.appendingPathComponent(Self.fileName)
    }

    private func load() {
        guard !loaded else { return }
        loaded = true
        guard let url = fileURL, let data = try? Data(contentsOf: url),
              let stored = try? JSONDecoder().decode(Stored.self, from: data)
        else { return }
        visits = stored.visits
    }

    private func loadItems() -> [Item] {
        guard let url = fileURL, let data = try? Data(contentsOf: url),
              let stored = try? JSONDecoder().decode(Stored.self, from: data)
        else { return [] }
        return stored.cookies
    }

    private func save(_ items: [Item]) {
        guard var url = fileURL,
              let data = try? JSONEncoder().encode(Stored(cookies: items, visits: visits))
        else { return }
        do {
            try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            // These are credentials for this device. They do not belong in a
            // backup that can be restored somewhere else.
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try? url.setResourceValues(values)
        } catch {
            NSLog("[CookieVault] could not save: \(error.localizedDescription)")
        }
    }

    /// Called when the user clears cookies by hand: the backup has to go with
    /// them, or the next sync would helpfully put everything back.
    func forgetEverything() {
        visits = [:]
        loaded = true
        lastSync = Date()
        if let url = fileURL { try? FileManager.default.removeItem(at: url) }
    }
}
