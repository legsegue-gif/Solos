import SwiftUI

/// The chat's model and its switches: every endpoint's models, fetched
/// from the endpoint, whether this session thinks, and whether it runs as
/// an agent (tools and the sandbox) or as plain chat.
struct ModelPicker: View {
    /// Kept current by the chat's events, so a change shows here at once.
    let session: SessionInfo
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var models: [String: [ModelInfo]] = [:]
    @State private var errors: [String: CoreError] = [:]
    @State private var refreshing = false
    @State private var error: CoreError?

    /// An endpoint that is turned off is not listed, and is not asked.
    private var enabledEndpoints: [Endpoint] { app.settings.endpoints.filter(\.enabled) }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Toggle(String(localized: "Thinking"), isOn: Binding(
                        get: { session.thinking ?? app.settings.thinking },
                        set: { on in Task { await setThinking(on) } }))
                } footer: {
                    Text("Whether the model thinks before answering in this chat. Some endpoints decide by model instead; their thinking is then hidden when this is off.")
                }
                Section {
                    Toggle(String(localized: "AI Agent Mode"), isOn: Binding(
                        get: { session.agentMode ?? app.settings.agentMode },
                        set: { on in Task { await setAgentMode(on) } }))
                } footer: {
                    Text("On: the model can run commands, read and write files, browse the web and use the device. Off: plain chat, the model only answers in text. Attached images are still sent.")
                }
                ForEach(enabledEndpoints, id: \.id) { ep in
                    Section(ep.name) {
                        if let e = errors[ep.id] {
                            Text(e.message).font(.caption).foregroundStyle(.red)
                        }
                        if let list = models[ep.id] {
                            ForEach(list, id: \.id) { m in
                                Button { Task { await choose(ep, m.id) } } label: {
                                    HStack {
                                        Text(m.id).foregroundStyle(Color.primary)
                                        Spacer()
                                        if session.model == ModelChoice(endpointId: ep.id, model: m.id) {
                                            Image(systemName: "checkmark").foregroundStyle(.tint)
                                        }
                                    }
                                }
                            }
                        } else if errors[ep.id] == nil {
                            ProgressView()
                        }
                    }
                }
                if let error {
                    Text(error.message).font(.caption).foregroundStyle(.red)
                }
            }
            .navigationTitle(String(localized: "Model"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    if refreshing {
                        ProgressView()
                    } else {
                        Button { Task { await refresh(all: true) } } label: { Image(systemName: "arrow.clockwise") }
                            .accessibilityLabel(String(localized: "Refresh models"))
                    }
                }
                ToolbarItem(placement: .confirmationAction) { Button(String(localized: "Done")) { dismiss() } }
            }
            .task { await load() }
        }
    }

    /// The lists kept from the last fetch show at once; an endpoint never
    /// listed, or listed long ago, is asked in the background.
    private func load() async {
        for ep in enabledEndpoints {
            if let kept = app.core?.cachedModels(endpointId: ep.id) { models[ep.id] = kept.models }
        }
        await refresh(all: false)
    }

    /// Ask the endpoints again: all of them, or only those with no list or an
    /// old one. A failure keeps the list on screen and shows nothing for it;
    /// without a list it is the endpoint's error.
    private func refresh(all: Bool) async {
        guard let core = app.core else { return }
        refreshing = all
        defer { refreshing = false }
        for ep in enabledEndpoints {
            if !all, let kept = core.cachedModels(endpointId: ep.id), !kept.stale { continue }
            do {
                models[ep.id] = try await core.listModels(endpoint: ep)
                errors[ep.id] = nil
            } catch let e as CoreError {
                if models[ep.id] == nil || all { errors[ep.id] = e }
            } catch {}
        }
    }

    private func choose(_ ep: Endpoint, _ model: String) async {
        do {
            _ = try await app.core?.setSessionModel(sessionId: session.id, model: ModelChoice(endpointId: ep.id, model: model))
            dismiss()
        } catch let e as CoreError {
            error = e
        } catch {}
    }

    private func setAgentMode(_ on: Bool) async {
        do {
            _ = try await app.core?.setSessionAgentMode(sessionId: session.id, agentMode: on)
        } catch let e as CoreError {
            error = e
        } catch {}
    }

    private func setThinking(_ on: Bool) async {
        do {
            _ = try await app.core?.setSessionThinking(sessionId: session.id, thinking: on)
        } catch let e as CoreError {
            error = e
        } catch {}
    }
}
