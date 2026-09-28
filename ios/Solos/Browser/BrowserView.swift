//  BrowserView.swift — seeing what the model is looking at.
//
//  The web view the browser tool drives is a real one, living offscreen so
//  pages lay out and screenshots render. This puts it on screen: the same
//  view, not a copy, so whatever the model just did is what you see, and
//  whatever you do next — scroll, log in, dismiss a cookie banner — is what
//  the model finds when it looks again.

import SwiftUI
import WebKit

struct BrowserView: View {
    @Environment(\.dismiss) private var dismiss
    @StateObject private var state = BrowserViewState()
    @State private var address = ""
    @State private var showSettings = false
    @State private var showDownloads = false
    @State private var breathing = false
    @FocusState private var editingAddress: Bool

    var body: some View {
        VStack(spacing: 0) {
            tabStrip
            Divider()
            if let page = state.page {
                addressBar(page)
                Divider()
                if state.held { takeoverBar(page) }
                ZStack {
                    PageView(page: page)
                    // A failed load used to be a white rectangle: no message,
                    // no way to try again, and nothing to tell it apart from a
                    // page that is simply blank.
                    if let message = state.error { loadFailure(message, page) }
                    // Two hands on one web view: while the model is driving,
                    // a tap of yours lands in the middle of its click and
                    // neither side knows why. The shade says who has it.
                    if state.busy, !state.held { modelShade(page) }
                }
            } else {
                VStack(spacing: 10) {
                    Image(systemName: "globe")
                        .font(.largeTitle)
                        .foregroundStyle(.tertiary)
                    Text("No page open").font(.headline)
                    Text("Type an address, or ask the model to open one. You both see the same page.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                    urlField
                        .padding(.top, 6)
                        .frame(maxWidth: 320)
                }
                .padding(32)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .navigationTitle(state.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button("Done") { dismiss() }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button("Browser settings", systemImage: "slider.horizontal.3") {
                        showSettings = true
                    }
                    Button("Downloads", systemImage: "arrow.down.circle") {
                        showDownloads = true
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        .sheet(isPresented: $showSettings) {
            NavigationStack {
                // Done belongs to the sheet; pushed from Settings it has a back button.
                BrowserSettingsView().toolbar {
                    ToolbarItem(placement: .topBarLeading) { Button("Done") { showSettings = false } }
                }
            }
        }
        .sheet(isPresented: $showDownloads) {
            NavigationStack { BrowserDownloadsView() }
        }
        .onAppear {
            TabSet.shared.restoreIfNeeded()
            state.refresh()
            address = state.host
        }
        // Taking over is for the minute it takes to log in. Walking away from
        // the browser hands every tab back, so the model is never locked out
        // by a switch nobody remembers flipping.
        .onDisappear { TabSet.shared.releaseAll() }
        // Follow the page while the user is not busy typing over it.
        .onChange(of: state.host) { host in
            if !editingAddress { address = host }
        }
    }

    /// One row, every tab, and a way to make another. Hiding this behind a
    /// menu that only appeared with two tabs open meant there was no way to
    /// get to two tabs.
    private var tabStrip: some View {
        HStack(spacing: 8) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(state.tabs, id: \.id) { tab in
                        tabChip(tab)
                    }
                }
                .padding(.horizontal, 2)
            }
            Text("\(state.tabs.count)/\(TabSet.maxPages)")
                .font(.caption2)
                .foregroundStyle(.tertiary)
                .monospacedDigit()
            Button {
                state.newTab()
                address = ""
                editingAddress = true
            } label: {
                Image(systemName: "plus")
            }
            .disabled(state.tabs.count >= TabSet.maxPages)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
    }

    private func tabChip(_ tab: BrowserViewState.TabSummary) -> some View {
        let active = tab.id == state.activeId
        return HStack(spacing: 4) {
            Text(tab.label)
                .font(.caption)
                .lineLimit(1)
                .frame(maxWidth: 130, alignment: .leading)
            Button { state.close(tab.id) } label: {
                Image(systemName: "xmark").font(.caption2)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(
            active ? Color.accentColor.opacity(0.18) : Color.secondary.opacity(0.1),
            in: Capsule()
        )
        .overlay(
            Capsule().strokeBorder(active ? Color.accentColor.opacity(0.5) : .clear)
        )
        .contentShape(Capsule())
        .onTapGesture { state.select(tab.id) }
    }

    private func addressBar(_ page: Page) -> some View {
        HStack(spacing: 12) {
            Button { page.web.goBack() } label: { Image(systemName: "chevron.left") }
                .disabled(!state.canGoBack)
            Button { page.web.goForward() } label: { Image(systemName: "chevron.right") }
                .disabled(!state.canGoForward)
            urlField
            // Always there, not only on the shade: the shade shows while one
            // action runs and is gone while the model thinks, so a button on
            // it alone came and went under the finger.
            if !state.held {
                Button { state.takeOver() } label: { Image(systemName: "hand.raised") }
                    .accessibilityLabel("Take over")
            }
            if state.isLoading {
                // A page that hangs used to be something you could only watch.
                Button { page.web.stopLoading() } label: {
                    Image(systemName: "xmark.circle")
                }
                .accessibilityLabel("Stop loading")
            } else {
                Button { page.web.reload() } label: { Image(systemName: "arrow.clockwise") }
                    .accessibilityLabel("Reload")
            }
        }
        .font(.callout)
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
    }

    /// The model has this tab. The shade is what makes it a handover rather
    /// than a race: it covers the page and swallows touches, so there is one
    /// driver at a time and you can see which.
    private func modelShade(_ page: Page) -> some View {
        ZStack {
            Color.black.opacity(0.06)
            VStack(spacing: 12) {
                HStack(spacing: 8) {
                    Circle()
                        .fill(Color.accentColor)
                        .frame(width: 8, height: 8)
                        .opacity(breathing ? 0.25 : 1)
                        .animation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true), value: breathing)
                    Text("Solos is browsing").font(.callout.weight(.medium))
                }
                Button("Take over") { state.takeOver() }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                Text("The tab is yours until you hand it back or close the browser; the model is kept out.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 260)
            }
            .padding(18)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
        }
        // Everything, including the taps that would otherwise reach the page.
        .contentShape(Rectangle())
        .onTapGesture {}
        .transition(.opacity)
        .onAppear { breathing = true }
    }

    private func takeoverBar(_ page: Page) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "hand.raised.fill").font(.caption)
            Text("You have this tab; the model's actions on it are refused").font(.caption)
            Spacer()
            Button("Hand back") { state.handBack() }
                .font(.caption.weight(.medium))
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 7)
        .background(Color.accentColor.opacity(0.14))
    }

    private func loadFailure(_ message: String, _ page: Page) -> some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle")
                .font(.largeTitle)
                .foregroundStyle(.tertiary)
            Text("This page could not be opened").font(.headline)
            Text(message)
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 300)
            Button("Retry") { state.retry() }
                .buttonStyle(.bordered)
                .controlSize(.small)
        }
        .padding(28)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(.systemBackground))
    }

    /// Typing here is the whole point of the browser being reachable on its
    /// own: without it you can only look at pages the model chose.
    private var urlField: some View {
        HStack(spacing: 6) {
            TextField("Address", text: $address)
                .font(.caption)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.URL)
                .submitLabel(.go)
                .focused($editingAddress)
                .onSubmit(open)
                .frame(maxWidth: .infinity, alignment: .leading)
            // Not only the keyboard's Go key: with a hardware keyboard
            // attached there is no Go key to press.
            Button(action: open) { Image(systemName: "arrow.right.circle.fill") }
                .font(.body)
                .disabled(address.trimmingCharacters(in: .whitespaces).isEmpty)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(Color.secondary.opacity(0.12), in: Capsule())
    }

    private func open() {
        state.go(address)
        editingAddress = false
    }
}

/// Follows the web view rather than owning it: the tool is still driving, so
/// the address bar has to track what the page does on its own.
@MainActor
final class BrowserViewState: ObservableObject {
    @Published private(set) var host = ""
    @Published private(set) var title = String(localized: "Browser")
    @Published private(set) var isLoading = false
    @Published private(set) var canGoBack = false
    @Published private(set) var canGoForward = false
    @Published private(set) var activeId = ""
    @Published private(set) var tabs: [TabSummary] = []
    /// What the last load said when it failed, for the overlay.
    @Published private(set) var error: String?
    /// Whether the model is driving this tab right now, and whether the
    /// person has taken it off it.
    @Published private(set) var busy = false
    @Published private(set) var held = false

    struct TabSummary {
        let id: String
        let label: String
    }

    nonisolated(unsafe) private var timer: Timer?

    var page: Page? {
        TabSet.shared.pages.first { $0.id == activeId } ?? TabSet.shared.pages.last
    }

    init() {
        // A second-by-second poll rather than KVO on four properties across a
        // changing set of web views: the view is only alive while someone is
        // looking at it, and this keeps the tool's side free of observers.
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refresh() }
        }
    }

    deinit { timer?.invalidate() }

    func select(_ id: String) {
        TabSet.shared.select(id)
        refresh()
    }

    func newTab() {
        let page = TabSet.shared.open().page
        activeId = page.id
        refresh()
    }

    func close(_ id: String) {
        TabSet.shared.close(id)
        refresh()
    }

    func takeOver() {
        TabSet.shared.takeOver(activeId)
        refresh()
    }

    func handBack() {
        TabSet.shared.handBack(activeId)
        refresh()
    }

    func retry() {
        page?.retry()
        refresh()
    }

    /// Load what the user typed, opening a tab if there is none.
    func go(_ text: String) {
        guard let url = Self.url(from: text) else { return }
        let target = page ?? TabSet.shared.open().page
        activeId = target.id
        TabSet.shared.select(target.id)
        target.web.load(URLRequest(url: url))
        refresh()
    }

    /// No search engine on purpose: a bare word is treated as a host, which
    /// is what an intranet name is, and nothing is sent anywhere unasked.
    static func url(from text: String) -> URL? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if let url = URL(string: trimmed), url.scheme != nil { return url }
        return URL(string: "https://\(trimmed)")
    }

    func refresh() {
        let set = TabSet.shared
        tabs = set.pages.map { page in
            TabSummary(
                id: page.id,
                label: page.web.title?.isEmpty == false
                    ? page.web.title!
                    : (page.web.url?.host() ?? page.id)
            )
        }
        if activeId.isEmpty || !set.pages.contains(where: { $0.id == activeId }) {
            activeId = set.activeId.isEmpty ? (set.pages.last?.id ?? "") : set.activeId
        }
        guard let page else { return }
        // A new tab shows an empty address, not the word for nothing.
        let current = page.web.url?.absoluteString ?? ""
        host = current == "about:blank" ? "" : current
        title = page.web.title?.isEmpty == false ? page.web.title! : String(localized: "Browser")
        isLoading = page.web.isLoading
        canGoBack = page.web.canGoBack
        canGoForward = page.web.canGoForward
        error = page.lastError
        busy = set.busy.contains(page.id)
        held = set.isHeld(page.id)
    }
}

/// Borrows the live web view for as long as the sheet is up, then hands it
/// back to the offscreen stage so the tool keeps working after it closes.
private struct PageView: UIViewRepresentable {
    let page: Page

    func makeUIView(context: Context) -> WKWebView {
        page.web.isUserInteractionEnabled = true
        page.web.alpha = 1
        return page.web
    }

    func updateUIView(_ view: WKWebView, context: Context) {}

    static func dismantleUIView(_ view: WKWebView, coordinator: ()) {
        Task { @MainActor in
            view.isUserInteractionEnabled = false
            if let page = TabSet.shared.pages.first(where: { $0.web === view }) {
                TabSet.shared.restage(page)
            }
        }
    }
}
