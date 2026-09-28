//  BrowserSettingsView.swift — the knobs a shared browser actually needs.
//
//  The model and the person holding the phone drive the same web views, so
//  these settings are not cosmetic: the user agent decides whether a site
//  serves its mobile or desktop markup, which decides what the model's page
//  snapshot contains. Cookies are the other half — they are what "log in
//  once, let the model carry on" is made of, so there has to be a way to see
//  how many there are and to throw them away.

import SwiftUI
import WebKit

struct BrowserSettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var agent = BrowserPrefs.agent
    @State private var cookieCount: Int?
    @State private var domains: [String] = []
    @State private var confirmClear = false

    var body: some View {
        Form {
            Section {
                Picker("User Agent", selection: $agent) {
                    ForEach(BrowserPrefs.Agent.allCases) { option in
                        Text(option.label).tag(option)
                    }
                }
                .pickerStyle(.segmented)
                Text(agent.note)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } header: {
                Text("Requests")
            } footer: {
                Text("Applies to open tabs too; reload to see the difference.")
            }

            Section {
                LabeledContent("Cookie") {
                    if let cookieCount {
                        Text("\(cookieCount)").monospacedDigit()
                    } else {
                        ProgressView().controlSize(.mini)
                    }
                }
                if !domains.isEmpty {
                    DisclosureGroup("Sites (\(domains.count))") {
                        ForEach(domains, id: \.self) { domain in
                            Text(domain).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
                Button("Clear cookies and website data", role: .destructive) { confirmClear = true }
                    .disabled(cookieCount == 0)
            } header: {
                Text("Cookies and data")
            } footer: {
                Text("This is what lets you log in once and the model carry on. Clearing it logs you out of every site.")
            }
        }
        .navigationTitle(String(localized: "Browser settings"))
        .navigationBarTitleDisplayMode(.inline)
        .onChange(of: agent) { new in BrowserPrefs.agent = new }
        .confirmationDialog("Clear cookies?", isPresented: $confirmClear, titleVisibility: .visible) {
            Button("Clear", role: .destructive) { clearCookies() }
        } message: {
            Text("Every site will log you out, and the model can no longer use those sessions.")
        }
        .task { await loadCookies() }
    }

    private func loadCookies() async {
        let store = WKWebsiteDataStore.default().httpCookieStore
        let cookies = await store.allCookies()
        cookieCount = cookies.count
        domains = Set(cookies.map { $0.domain }).sorted()
    }

    private func clearCookies() {
        // The backup has to go with them: otherwise the next browser action
        // would helpfully restore everything the user just asked to be rid of.
        CookieVault.shared.forgetEverything()
        let types = WKWebsiteDataStore.allWebsiteDataTypes()
        WKWebsiteDataStore.default().removeData(ofTypes: types, modifiedSince: .distantPast) {
            Task { await loadCookies() }
        }
    }
}
