//  BrowserTools.swift — the platform half of the browser tool (spec §7.1).
//
//  The core owns everything the model sees: the page snapshot, clicking and
//  typing by number, scrolling. All of that is JavaScript sent down from the
//  core, so this file only has to load a URL, run a script, take a picture
//  and keep a list of tabs.
//
//  Calls arrive on a core thread. A web view lives on the main thread and
//  answers through callbacks, so each operation is started on main and the
//  calling thread waits for the callback with a deadline.

import Foundation
import UIKit
import WebKit

final class BrowserTools: BrowserHost, @unchecked Sendable {
    private let tabs = TabSet.shared

    func call(op: String, inputJson: String) -> String {
        let input = (try? JSONSerialization.jsonObject(with: Data(inputJson.utf8))) as? [String: Any] ?? [:]
        let tab = input["tab"] as? String ?? ""
        let result: [String: Any]
        switch op {
        case "navigate":
            result = navigate(url: input["url"] as? String ?? "", tab: tab)
        case "eval":
            result = eval(js: input["js"] as? String ?? "", tab: tab)
        case "settle":
            result = settle(tab: tab)
        case "screenshot":
            result = screenshot(tab: tab, fullPage: input["full_page"] as? Bool ?? false)
        case "tabs":
            result = manageTabs(input, tab: tab)
        case "user_agent":
            result = setUserAgent(input["profile"] as? String ?? "default")
        case "viewport":
            result = setViewport(input, tab: tab)
        case "cookies":
            result = cookies(input)
        case "downloads":
            result = manageDownloads(input)
        case "cancel":
            result = cancelInFlight(tab: tab)
        default:
            result = ["error": "unknown browser operation: \(op)"]
        }
        let data = (try? JSONSerialization.data(withJSONObject: result))
            ?? Data(#"{"error":"the result could not be serialised"}"#.utf8)
        return String(decoding: data, as: UTF8.self)
    }

    // MARK: - Operations

    private func navigate(url: String, tab: String) -> [String: Any] {
        guard let target = URL(string: url) else { return ["error": "not a URL: \(url)"] }
        return waiting(seconds: 45, tab: tab, notices: true) { done in
            guard let page = self.tabs.page(tab) else { return done(["error": "no tab is available"]) }
            page.load(target) { result in
                switch result {
                case .success(let outcome):
                    var landed: [String: Any] = ["url": page.web.url?.absoluteString ?? url, "title": page.web.title ?? ""]
                    if outcome == .stillLoading { landed["still_loading"] = true }
                    done(landed)
                case .failure(let error):
                    done(["error": "could not open \(url): \(error.localizedDescription)"])
                }
            }
        }
    }

    private func eval(js: String, tab: String) -> [String: Any] {
        // A click that navigated tears down the context the script was headed
        // for. That reads as a failure but only means "try the new page".
        for attempt in 0..<2 {
            let result = waiting(seconds: 30, tab: tab) { done in
                guard let page = self.tabs.page(tab) else { return done(["error": "no tab is available"]) }
                page.web.callAsyncJavaScript(js, arguments: [:], in: nil, in: .defaultClient) { outcome in
                    switch outcome {
                    case .success(let value):
                        done(["value": value as? String ?? String(describing: value)])
                    case .failure(let error):
                        done(["error": "the page script failed: \(error.localizedDescription)"])
                    }
                }
            }
            if result["value"] != nil || attempt == 1 { return result }
            Thread.sleep(forTimeInterval: 0.4)
        }
        return ["error": "the page script returned nothing"]
    }

    /// Wait out whatever the last action started. A click that navigates
    /// needs this; a click that only ran a handler comes back in the grace
    /// period. Either way the caller learns when the page is worth reading.
    private func settle(tab: String) -> [String: Any] {
        waiting(seconds: 25, tab: tab, notices: true) { done in
            guard let page = self.tabs.page(tab) else { return done(["error": "no tab is available"]) }
            page.settle { done(["url": page.web.url?.absoluteString ?? "", "title": page.web.title ?? ""]) }
        }
    }

    private func screenshot(tab: String, fullPage: Bool) -> [String: Any] {
        waiting(seconds: 40, tab: tab) { done in
            guard let page = self.tabs.page(tab) else { return done(["error": "no tab is available"]) }
            let shoot: (CGSize?) -> Void = { size in
                let config = WKSnapshotConfiguration()
                config.afterScreenUpdates = true
                if let size { config.rect = CGRect(origin: .zero, size: size) }
                page.web.takeSnapshot(with: config) { image, error in
                    if size != nil { self.tabs.restage(page) }
                    guard let image, let png = image.pngData() else {
                        return done(["error": "the screenshot failed: \(error?.localizedDescription ?? "no image")"])
                    }
                    done([
                        "base64": png.base64EncodedString(),
                        "width": Int(image.size.width),
                        "height": Int(image.size.height),
                    ])
                }
            }
            guard fullPage else { return shoot(nil) }
            // The whole scrollable page: grow the view to the document's own
            // height, take the picture, and put it back. A long article is
            // otherwise three or four screenshots the model has to stitch.
            page.web.evaluateJavaScript("document.documentElement.scrollHeight") { value, _ in
                let height = min(CGFloat((value as? NSNumber)?.doubleValue ?? 0), 8_000)
                guard height > page.web.bounds.height else { return shoot(nil) }
                let size = CGSize(width: page.web.bounds.width, height: height)
                page.web.frame = CGRect(origin: page.web.frame.origin, size: size)
                page.web.setNeedsLayout()
                page.web.layoutIfNeeded()
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { shoot(size) }
            }
        }
    }

    /// What the site is told it is talking to. Applied to every open tab and
    /// remembered, so it survives the next tab and the next launch.
    private func setUserAgent(_ profile: String) -> [String: Any] {
        let agent: BrowserPrefs.Agent
        switch profile {
        case "desktop": agent = .desktop
        default: agent = .mobile
        }
        return waiting(seconds: 10) { done in
            // The setter already pushes it to every open tab and remembers it
            // for the next launch.
            BrowserPrefs.agent = agent
            done(["user_agent": agent.string, "profile": profile])
        }
    }

    /// The window the page believes it is in. A layout that hides its
    /// navigation below 900pt hides it from the model too.
    private func setViewport(_ input: [String: Any], tab: String) -> [String: Any] {
        let reset = input["reset"] as? Bool ?? false
        let width = CGFloat((input["width"] as? NSNumber)?.doubleValue ?? 0)
        let height = CGFloat((input["height"] as? NSNumber)?.doubleValue ?? 0)
        return waiting(seconds: 10, tab: tab) { done in
            guard let page = self.tabs.page(tab) else { return done(["error": "no tab is available"]) }
            if reset {
                self.tabs.restage(page)
            } else {
                page.web.frame = CGRect(x: page.web.frame.origin.x, y: page.web.frame.origin.y,
                                        width: max(width, 200), height: max(height, 200))
                page.web.setNeedsLayout()
                page.web.layoutIfNeeded()
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                done(["width": Int(page.web.bounds.width), "height": Int(page.web.bounds.height)])
            }
        }
    }

    /// Read, write or clear cookies. This is the seam between a login the
    /// person made by hand and the model carrying on in the same session.
    private func cookies(_ input: [String: Any]) -> [String: Any] {
        let operation = input["operation"] as? String ?? "get"
        let domain = (input["domain"] as? String ?? "").lowercased()
        let wanted = input["cookies"] as? [[String: Any]] ?? []
        let matches: @Sendable (HTTPCookie) -> Bool = { cookie in
            domain.isEmpty || cookie.domain.lowercased().hasSuffix(domain)
        }
        let valid = wanted.compactMap(Self.cookie(from:))
        return waiting(seconds: 20) { done in
            let store = WKWebsiteDataStore.default().httpCookieStore
            switch operation {
            case "set":
                guard !valid.isEmpty else { return done(["changed": 0]) }
                let tally = Tally(valid.count)
                for cookie in valid {
                    store.setCookie(cookie) {
                        if tally.tick() { done(["changed": valid.count]) }
                    }
                }
            case "clear":
                store.getAllCookies { all in
                    let victims = all.filter(matches)
                    guard !victims.isEmpty else { return done(["changed": 0]) }
                    let tally = Tally(victims.count)
                    for cookie in victims {
                        store.delete(cookie) {
                            if tally.tick() { done(["changed": victims.count]) }
                        }
                    }
                }
            default:
                store.getAllCookies { all in
                    let rows = all.filter(matches).map { cookie -> [String: Any] in
                        var row: [String: Any] = [
                            "name": cookie.name,
                            "value": cookie.value,
                            "domain": cookie.domain,
                            "path": cookie.path,
                            "secure": cookie.isSecure,
                            "http_only": cookie.isHTTPOnly,
                        ]
                        if let expires = cookie.expiresDate {
                            row["expires"] = Int(expires.timeIntervalSince1970)
                        }
                        return row
                    }
                    done(["cookies": rows])
                }
            }
        }
    }

    private static func cookie(from row: [String: Any]) -> HTTPCookie? {
        guard let name = row["name"] as? String, let value = row["value"] as? String else { return nil }
        var properties: [HTTPCookiePropertyKey: Any] = [
            .name: name,
            .value: value,
            .domain: row["domain"] as? String ?? "",
            .path: row["path"] as? String ?? "/",
        ]
        if (row["secure"] as? Bool) == true { properties[.secure] = "TRUE" }
        if (row["http_only"] as? Bool) == true { properties[HTTPCookiePropertyKey("HttpOnly")] = "TRUE" }
        if let expires = (row["expires"] as? NSNumber)?.doubleValue {
            properties[.expires] = Date(timeIntervalSince1970: expires)
        }
        return HTTPCookie(properties: properties)
    }

    /// List downloads, or stop one. Files land in the workspace, so a
    /// finished download needs nothing further from this tool.
    private func manageDownloads(_ input: [String: Any]) -> [String: Any] {
        let operation = input["operation"] as? String ?? "list"
        let id = input["id"] as? String ?? ""
        return waiting(seconds: 10) { done in
            MainActor.assumeIsolated {
                let center = DownloadCenter.shared
                if operation == "cancel", !id.isEmpty { center.cancel(id) }
                // Reporting the whole list also clears the pending notices:
                // the model has just been told everything there is to tell.
                _ = center.drainNotices()
                done(["downloads": center.rows()])
            }
        }
    }

    private func manageTabs(_ input: [String: Any], tab: String) -> [String: Any] {
        let operation = input["operation"] as? String ?? "list"
        let url = input["url"] as? String ?? ""
        let acts = operation == "close" || operation == "select"
        let listing = waiting(seconds: 10, tab: acts ? tab : nil) { done in
            switch operation {
            case "open":
                let (page, evicted) = self.tabs.open()
                if let target = URL(string: url), !url.isEmpty {
                    page.load(target) { _ in done(self.tabs.listing(evicted: evicted)) }
                    return
                }
                done(self.tabs.listing(evicted: evicted))
                return
            case "close":
                self.tabs.close(tab)
            case "select":
                self.tabs.select(tab)
            default:
                break
            }
            done(self.tabs.listing())
        }
        return listing
    }

    // MARK: - Cancelling

    /// Waits that have not answered yet, keyed by a ticket. Only ever touched
    /// on the main thread, which is also where the waits are completed — that
    /// is what makes letting one go from another thread safe.
    nonisolated(unsafe) private static var inFlight: [Int: (tab: String, release: (String) -> Void)] = [:]
    nonisolated(unsafe) private static var ticket = 0

    /// Let go of whatever is in flight on a tab, and stop the page loading.
    ///
    /// Sent by the core when the user presses Stop. It arrives on a different
    /// thread from the call it is cancelling and must not wait for it: the
    /// point is to unblock that call, not to queue behind it. Before this,
    /// Stop ended the turn and left the web view loading — a click already
    /// handed over still landed, up to forty-five seconds later, under the
    /// hands of whoever had just taken the tab over.
    /// The person took the tab: whatever the model was doing on it ends
    /// now, and it is told why, rather than finishing a click on a page
    /// someone else is using. Called on main.
    static func releaseInFlight(tab: String) {
        for (id, entry) in inFlight where entry.tab == tab {
            entry.release("The person has taken over tab \(tab); this action was stopped. It cannot be driven until they hand it back.")
            inFlight[id] = nil
        }
    }

    private func cancelInFlight(tab: String) -> [String: Any] {
        DispatchQueue.main.async {
            let wanted = self.tabs.resolvedId(tab)
            for (id, entry) in Self.inFlight where entry.tab.isEmpty || wanted.isEmpty || entry.tab == wanted {
                entry.release("cancelled")
                Self.inFlight[id] = nil
            }
            // The load itself, which no semaphore knows about.
            for page in self.tabs.pages where wanted.isEmpty || page.id == wanted {
                page.web.stopLoading()
                self.tabs.markBusy(page.id, false)
            }
        }
        return ["cancelled": true]
    }

    // MARK: - Waiting

    /// Start `work` on the main thread and block until it reports back or the
    /// deadline passes. Safe because these calls always arrive on a core
    /// thread, never on main.
    ///
    /// `tab` names the page the operation drives, which gives this one place
    /// two more jobs: refuse the call outright while the person has that tab
    /// taken over, and mark it busy for as long as the model is working on it
    /// so the browser view can shade it. `notices` carries back downloads
    /// that began or ended since the last time anyone asked.
    private func waiting(
        seconds: TimeInterval,
        tab: String? = nil,
        notices: Bool = false,
        _ work: @escaping @Sendable (@escaping @Sendable ([String: Any]) -> Void) -> Void
    ) -> [String: Any] {
        let semaphore = DispatchSemaphore(value: 0)
        let box = ResultBox()
        DispatchQueue.main.async {
            // Every operation goes through here, so this is the one place the
            // model's side can wake the saved tabs — on main, where the tab
            // set lives, and without a blocking hop that could deadlock.
            self.tabs.restoreIfNeeded()
            // …and the one place to notice that the system has thrown away a
            // login since the last time we looked.
            MainActor.assumeIsolated { CookieVault.shared.syncIfNeeded() }

            var driving = ""
            if let tab {
                driving = self.tabs.resolvedId(tab)
                if self.tabs.isHeld(driving) {
                    box.finished = true
                    box.value = ["error": "The person has taken over tab \(driving); it cannot be driven until they hand it back. Wait, or use another tab."]
                    semaphore.signal()
                    return
                }
                self.tabs.markBusy(driving, true)
            }
            // Registered only once the wait is real: a call refused above has
            // already answered and must not be released twice.
            Self.ticket += 1
            let ticket = Self.ticket
            box.ticket = ticket
            Self.inFlight[ticket] = (tab: driving, release: { reason in
                guard !box.finished else { return }
                box.finished = true
                self.tabs.markBusy(driving, false)
                box.value = ["error": reason]
                semaphore.signal()
            })
            let myTicket = ticket
            let myDriving = driving
            work { result in
                let ticket = myTicket
                let driving = myDriving
                Self.inFlight[ticket] = nil
                guard !box.finished else { return }
                box.finished = true
                self.tabs.markBusy(driving, false)
                var result = result
                if notices {
                    nonisolated(unsafe) var rows: [[String: Any]] = []
                    MainActor.assumeIsolated { rows = DownloadCenter.shared.drainNotices() }
                    if !rows.isEmpty { result["downloads"] = rows }
                }
                box.value = result
                semaphore.signal()
            }
        }
        if semaphore.wait(timeout: .now() + seconds) == .timedOut {
            // The work may still be running; the shade must not outlive the
            // call the user can see the result of.
            DispatchQueue.main.async {
                Self.inFlight[box.ticket] = nil
                if let tab { self.tabs.markBusy(self.tabs.resolvedId(tab), false) }
            }
            return ["error": "the browser did not finish within \(Int(seconds)) seconds"]
        }
        return box.value
    }
}

/// Written and read only on the main thread until the semaphore is signalled,
/// which is what makes the hand-off safe.
/// State of one wait, touched only on the main queue until the waiting
/// thread reads the value after the semaphore.
private final class ResultBox: @unchecked Sendable {
    var finished = false
    var ticket = 0
    var value: [String: Any] = ["error": "the browser did not answer"]
}

/// Counts callbacks down on the main queue; true on the last one.
private final class Tally: @unchecked Sendable {
    private var left: Int
    init(_ count: Int) { left = count }
    func tick() -> Bool {
        left -= 1
        return left == 0
    }
}

// MARK: - Tabs

/// The set of open pages. Everything here runs on the main thread.
final class TabSet: @unchecked Sendable {
    static let shared = TabSet()

    /// Spec §5: at most three. Each page is a separate web content process
    /// with its own memory, and a model asked to compare things will happily
    /// open twenty.
    static let maxPages = 3

    private(set) var pages: [Page] = []
    private(set) var activeId: String = ""
    private var counter = 0
    /// When each page was last asked for, so the one to drop is the one
    /// nobody has looked at in longest.
    private var touched: [String: Date] = [:]

    /// Tabs the model is driving right now. The browser view puts a shade
    /// over these: two hands on one web view means the person's tap lands in
    /// the middle of the model's click and neither side knows why.
    private(set) var busy: Set<String> = []
    /// Tabs the person has taken over. The model is refused on these until
    /// they hand it back — usually they are in the middle of logging in.
    private(set) var held: Set<String> = []

    /// Where an evicted tab was, so its URL is not simply lost. The cap is
    /// three, and a model comparing four pages will hit it; being told which
    /// address went away is the difference between reopening it and going
    /// through history to look for it.
    private(set) var closedURLs: [(url: String, title: String)] = []

    func markBusy(_ id: String, _ on: Bool) {
        guard !id.isEmpty else { return }
        if on { busy.insert(id) } else { busy.remove(id) }
    }

    func isHeld(_ id: String) -> Bool { held.contains(id) }

    func takeOver(_ id: String) {
        guard !id.isEmpty else { return }
        held.insert(id)
        busy.remove(id)
        BrowserTools.releaseInFlight(tab: id)
    }

    func handBack(_ id: String) { held.remove(id) }

    /// Closing the browser view hands everything back: taking over is for the
    /// minute it takes to log in, not a mode to be left switched on.
    func releaseAll() { held.removeAll() }

    /// Installed by the client at startup. History lives in the core's store,
    /// and every page load comes through here whether the model or the person
    /// holding the phone asked for it.
    var onVisit: ((_ url: String, _ title: String) -> Void)?

    /// Where the open tabs were, so closing the app does not throw away what
    /// you were in the middle of — a page you logged into by hand most of all.
    private static let savedKey = "browser.tabs"
    private var restored = false
    private var restoring = false

    private struct Saved: Codable {
        var urls: [String]
        var active: Int
    }

    /// Called before the first use from either side, so the app's launch is
    /// not slowed by web views nobody has asked for yet.
    func restoreIfNeeded() {
        guard !restored else { return }
        restored = true
        guard let data = UserDefaults.standard.data(forKey: Self.savedKey),
              let saved = try? JSONDecoder().decode(Saved.self, from: data),
              !saved.urls.isEmpty
        else { return }

        restoring = true
        for address in saved.urls.prefix(Self.maxPages) {
            guard let url = URL(string: address) else { continue }
            let page = open().page
            page.web.load(URLRequest(url: url))
        }
        if saved.active >= 0, saved.active < pages.count {
            activeId = pages[saved.active].id
        }
        restoring = false
    }

    func persist() {
        guard !restoring else { return }
        var urls: [String] = []
        var active = 0
        for page in pages {
            let address = page.web.url?.absoluteString ?? ""
            guard !address.isEmpty, address != "about:blank" else { continue }
            if page.id == activeId { active = urls.count }
            urls.append(address)
        }
        let defaults = UserDefaults.standard
        guard !urls.isEmpty else {
            defaults.removeObject(forKey: Self.savedKey)
            return
        }
        if let data = try? JSONEncoder().encode(Saved(urls: urls, active: active)) {
            defaults.set(data, forKey: Self.savedKey)
        }
    }

    /// A host view kept in the app's window so pages lay out and render even
    /// while nobody is looking at them. A web view with no window produces
    /// blank snapshots and, on some sites, no layout at all.
    /// Only ever touched on the main thread.
    private lazy var stage: UIView = MainActor.assumeIsolated {
        let view = UIView()
        view.isUserInteractionEnabled = false
        view.alpha = 0.02
        if let window = Self.hostWindow {
            view.frame = window.bounds
            window.insertSubview(view, at: 0)
        } else {
            view.frame = CGRect(x: 0, y: 0, width: 390, height: 844)
        }
        return view
    }

    @MainActor static var hostWindow: UIWindow? {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .first { $0.isKeyWindow }
    }

    /// Which page an operation would act on, without opening one to find out.
    /// Empty means nothing is open yet.
    func resolvedId(_ id: String) -> String {
        if !id.isEmpty, pages.contains(where: { $0.id == id }) { return id }
        if pages.contains(where: { $0.id == activeId }) { return activeId }
        return pages.last?.id ?? ""
    }

    func page(_ id: String) -> Page? {
        if !id.isEmpty, let found = pages.first(where: { $0.id == id }) {
            touched[found.id] = Date()
            return found
        }
        if let active = pages.first(where: { $0.id == activeId }) {
            touched[active.id] = Date()
            return active
        }
        return pages.isEmpty ? open().page : pages.first
    }

    /// Open a tab, dropping the least recently used one if that would take
    /// the set over the cap. The id of anything dropped comes back, so the
    /// caller can say so rather than leave the model holding a dead tab.
    @discardableResult
    func open() -> (page: Page, evicted: String?) {
        var evicted: String?
        while pages.count >= Self.maxPages, let victim = leastRecentlyUsed() {
            evicted = victim
            close(victim)
        }
        counter += 1
        let page = Page(id: "tab\(counter)", frame: stage.bounds)
        stage.addSubview(page.web)
        pages.append(page)
        activeId = page.id
        touched[page.id] = Date()
        return (page, evicted)
    }

    /// Take on a page the site asked for, built from WebKit's configuration
    /// rather than a fresh one. At the cap this drops the least recently used
    /// tab, same as any other new tab — except never the opener, which is the
    /// one page the popup is talking to. Nil means even that left no room, and
    /// the caller should fall back to loading in the current tab.
    func adopt(configuration: WKWebViewConfiguration, opener: Page) -> Page? {
        if pages.count >= Self.maxPages {
            guard let victim = leastRecentlyUsed(excluding: opener.id) else { return nil }
            close(victim)
        }
        counter += 1
        let page = Page(id: "tab\(counter)", frame: stage.bounds, configuration: configuration)
        stage.addSubview(page.web)
        pages.append(page)
        activeId = page.id
        touched[page.id] = Date()
        return page
    }

    private func leastRecentlyUsed(excluding keep: String? = nil) -> String? {
        pages
            .filter { $0.id != keep }
            .min { touched[$0.id] ?? .distantPast < touched[$1.id] ?? .distantPast }?
            .id
    }

    func close(_ id: String) {
        let target = id.isEmpty ? activeId : id
        guard let index = pages.firstIndex(where: { $0.id == target }) else { return }
        let page = pages[index]
        if let url = page.web.url?.absoluteString, url != "about:blank" {
            closedURLs.removeAll { $0.url == url }
            closedURLs.insert((url, page.web.title ?? ""), at: 0)
            closedURLs = Array(closedURLs.prefix(5))
        }
        page.web.removeFromSuperview()
        pages.remove(at: index)
        touched[target] = nil
        busy.remove(target)
        held.remove(target)
        if activeId == target { activeId = pages.last?.id ?? "" }
        persist()
    }

    func select(_ id: String) {
        guard pages.contains(where: { $0.id == id }) else { return }
        activeId = id
        touched[id] = Date()
        persist()
    }

    /// Put a page back on the offscreen stage after a viewer borrowed it.
    func restage(_ page: Page) {
        guard page.web.superview !== stage else { return }
        page.web.frame = stage.bounds
        stage.addSubview(page.web)
    }

    func listing(evicted: String? = nil) -> [String: Any] {
        var out: [String: Any] = [
            "tabs": pages.map { page in
                [
                    "id": page.id,
                    "url": page.web.url?.absoluteString ?? "about:blank",
                    "title": page.web.title ?? "",
                    "active": page.id == activeId,
                ] as [String: Any]
            },
            "active": activeId,
        ]
        if let evicted {
            out["note"] = "At most \(Self.maxPages) tabs: \(evicted), the longest unused, was closed."
        }
        if !closedURLs.isEmpty {
            out["closed"] = closedURLs.map { ["url": $0.url, "title": $0.title] }
        }
        return out
    }
}

/// One tab: a web view plus the one thing a web view cannot report on its
/// own, which is when a load has finished.
/// How a load that did not fail came to an end.
enum LoadOutcome {
    /// WebKit said the page finished loading.
    case finished
    /// The page is on screen but has not stopped loading, and waiting any
    /// longer would only delay what is already there (see `Page.softLimit`).
    case stillLoading
}

final class Page: NSObject, WKNavigationDelegate, WKUIDelegate {
    let id: String
    let web: WKWebView
    private var pending: ((Result<LoadOutcome, Error>) -> Void)?
    /// The current load has drawn something: its first response committed.
    private var committed = false
    /// `softLimit` has passed for the current load.
    private var pastSoftLimit = false
    private var softDeadline: DispatchWorkItem?

    /// What went wrong the last time this page tried to load, for the error
    /// overlay. Without it a failed load is a white rectangle: no message, no
    /// way to try again, and no way to tell it from a page that is merely blank.
    private(set) var lastError: String?
    /// Where to go back to — after a failure, and after the web content
    /// process is killed, when the view no longer knows its own URL.
    private(set) var lastURL: URL?
    /// Set between "this response is a download" and the provisional-load
    /// failure WebKit reports immediately afterwards, which is not a failure.
    private var becameDownload = false

    /// `configuration` is non-nil only for a page the site itself asked for.
    /// WebKit hands over the configuration the child must be built with, and
    /// it is the only thing that ties the two together — build the child from
    /// a fresh one and `window.opener` is null on the other side.
    init(id: String, frame: CGRect, configuration: WKWebViewConfiguration? = nil) {
        self.id = id
        let config = configuration ?? {
            let fresh = WKWebViewConfiguration()
            fresh.defaultWebpagePreferences.allowsContentJavaScript = true
            // The default store keeps cookies, so a site the user logged into
            // stays logged in across turns and across launches.
            fresh.websiteDataStore = .default()
            return fresh
        }()
        let size = frame.width > 1 ? frame : CGRect(x: 0, y: 0, width: 390, height: 844)
        web = WKWebView(frame: size, configuration: config)
        web.allowsBackForwardNavigationGestures = false
        // Whatever the user picked in the browser settings, from the start:
        // a page loaded with the wrong UA has already chosen its markup.
        web.customUserAgent = BrowserPrefs.agent.string
        super.init()
        web.navigationDelegate = self
        web.uiDelegate = self
    }

    /// How long a click gets to turn into a navigation before the page is
    /// declared quiet, and how long that navigation then gets to finish.
    private static let grace: TimeInterval = 0.5
    private static let limit: TimeInterval = 15

    /// Call back once the page has stopped loading. Polls because a click is
    /// not a navigation, so there is nothing to be a delegate of until one
    /// actually starts.
    func settle(completion: @escaping () -> Void) {
        let start = Date()
        func poll() {
            let elapsed = Date().timeIntervalSince(start)
            if web.isLoading {
                guard elapsed < Self.limit else { return completion() }
            } else if elapsed >= Self.grace {
                return completion()
            }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1, execute: poll)
        }
        poll()
    }

    /// How long a load gets to finish before whatever it has drawn is handed
    /// over as it stands. Some pages never finish: X keeps connections open
    /// and loads for as long as the tab is there, so waiting for `didFinish`
    /// turned every tweet into "超过 45 秒没有完成" with the post already on
    /// screen (2026-09-25, on device). After 30 s the page is handed over
    /// with a note that it may be partial. A load that has drawn
    /// nothing at all by then is still waited for, and still fails.
    static let softLimit: TimeInterval = 30

    func load(_ url: URL, completion: @escaping (Result<LoadOutcome, Error>) -> Void) {
        finish(.failure(BrowserError.superseded))
        pending = completion
        lastError = nil
        lastURL = url
        committed = false
        pastSoftLimit = false
        let deadline = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.pastSoftLimit = true
            if self.committed { self.finish(.success(.stillLoading)) }
        }
        softDeadline = deadline
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.softLimit, execute: deadline)
        web.load(URLRequest(url: url))
    }

    /// Try the failed load again. `reload()` alone is not enough: a load that
    /// never committed leaves the view with nothing to reload.
    func retry() {
        lastError = nil
        if web.url != nil {
            web.reload()
        } else if let lastURL {
            web.load(URLRequest(url: lastURL))
        }
    }

    private func finish(_ result: Result<LoadOutcome, Error>) {
        guard let pending else { return }
        self.pending = nil
        softDeadline?.cancel()
        softDeadline = nil
        pending(result)
    }

    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
        lastError = nil
    }

    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
        lastError = nil
        lastURL = webView.url ?? lastURL
        committed = true
        // Drawn only after the limit: nothing to wait for any more.
        if pastSoftLimit { finish(.success(.stillLoading)) }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        lastError = nil
        lastURL = webView.url ?? lastURL
        if let url = webView.url?.absoluteString {
            TabSet.shared.onVisit?(url, webView.title ?? "")
            TabSet.shared.persist()
        }
        // Ageing in the cookie backup is keyed on real navigations.
        MainActor.assumeIsolated { CookieVault.shared.noteVisit(webView.url) }
        finish(.success(.finished))
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        fail(error)
    }

    func webView(
        _ webView: WKWebView,
        didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        fail(error)
    }

    /// One place for both failure callbacks, because two of the things that
    /// arrive here are not failures: the interrupt WebKit reports when a
    /// response turned into a download, and a load the user or the next
    /// action cancelled.
    private func fail(_ error: Error) {
        if becameDownload {
            becameDownload = false
            return finish(.success(.finished))
        }
        let ns = error as NSError
        let cancelled = ns.domain == NSURLErrorDomain && ns.code == NSURLErrorCancelled
        if !cancelled {
            lastError = error.localizedDescription
        }
        finish(.failure(error))
    }

    /// A response the web view cannot display is a file the user wants. This
    /// is also where `Content-Disposition: attachment` is honoured — WebKit
    /// will happily render a PDF the site meant you to save.
    func webView(
        _ webView: WKWebView,
        decidePolicyFor navigationResponse: WKNavigationResponse,
        decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void
    ) {
        let http = navigationResponse.response as? HTTPURLResponse
        let disposition = (http?.value(forHTTPHeaderField: "Content-Disposition") ?? "").lowercased()
        if !navigationResponse.canShowMIMEType || disposition.hasPrefix("attachment") {
            decisionHandler(.download)
        } else {
            decisionHandler(.allow)
        }
    }

    func webView(
        _ webView: WKWebView,
        navigationResponse: WKNavigationResponse,
        didBecome download: WKDownload
    ) {
        becameDownload = true
        // Whoever asked for this navigation is waiting on a page that is
        // never going to arrive. Release them now rather than leave the call
        // to time out — WebKit's own "frame load interrupted" may or may not
        // follow, and either way it is not an error.
        finish(.success(.finished))
        MainActor.assumeIsolated { DownloadCenter.shared.adopt(download) }
    }

    /// A link with a `download` attribute, or a blob the page built itself:
    /// there is no response to decide about, WebKit just hands one over.
    func webView(
        _ webView: WKWebView,
        navigationAction: WKNavigationAction,
        didBecome download: WKDownload
    ) {
        becameDownload = true
        finish(.success(.finished))
        MainActor.assumeIsolated { DownloadCenter.shared.adopt(download) }
    }

    /// The web content process was killed — usually the system reclaiming
    /// memory. The view stays in the hierarchy showing nothing at all, and
    /// every script sent to it fails, so put the page back rather than let
    /// the model work against a corpse.
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        if webView.url != nil {
            webView.reload()
        } else if let lastURL {
            webView.load(URLRequest(url: lastURL))
        }
        // Set after asking for the reload, not before: the message stays up
        // only if the reload never starts. If it does, the first delegate
        // callback clears it and the page comes back with nothing to explain.
        lastError = "The system reclaimed the page's process; reloading."
    }

    /// A link or a script that asks for a new window gets a real one, built
    /// from the configuration WebKit passes in.
    ///
    /// This used to fold the popup into the current tab and return nil, which
    /// looks fine until a sign-in flow needs it: the child then has no
    /// `window.opener` to post its result back to and no window to close, so
    /// every "log in with Google/GitHub" popup dead-ends. Returning the child
    /// synchronously is what keeps the pair related — and WebKit performs the
    /// navigation itself, so nothing is loaded here.
    ///
    /// When the tab cap leaves no room, falling back to the current tab is
    /// better than refusing: a redirect-based flow still completes.
    func webView(
        _ webView: WKWebView,
        createWebViewWith configuration: WKWebViewConfiguration,
        for navigationAction: WKNavigationAction,
        windowFeatures: WKWindowFeatures
    ) -> WKWebView? {
        if let child = TabSet.shared.adopt(configuration: configuration, opener: self) {
            return child.web
        }
        if navigationAction.targetFrame == nil, navigationAction.request.url != nil {
            webView.load(navigationAction.request)
        }
        return nil
    }

    /// The page called `window.close()` — an OAuth popup saying it is done.
    func webViewDidClose(_ webView: WKWebView) {
        TabSet.shared.close(id)
    }
}

enum BrowserError: LocalizedError {
    case superseded
    var errorDescription: String? { "the previous load was replaced by a newer one" }
}
