import SwiftUI

/// The chat's model and thinking switch: every endpoint's models, fetched
/// from the endpoint, and whether this session thinks.
struct ModelPicker: View {
    /// Kept current by the chat's events, so a change shows here at once.
    let session: SessionInfo
    @Environment(AppCore.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var models: [String: [ModelInfo]] = [:]
    @State private var errors: [String: CoreError] = [:]
    @State private var error: CoreError?

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
                ForEach(app.settings.endpoints, id: \.id) { ep in
                    Section(ep.name) {
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
                        } else if let e = errors[ep.id] {
                            Text(e.message).font(.caption).foregroundStyle(.red)
                        } else {
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
                ToolbarItem(placement: .confirmationAction) { Button(String(localized: "Done")) { dismiss() } }
            }
            .task { await load() }
        }
    }

    private func load() async {
        guard let core = app.core else { return }
        for ep in app.settings.endpoints {
            do {
                models[ep.id] = try await core.listModels(endpoint: ep)
            } catch let e as CoreError {
                errors[ep.id] = e
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

    private func setThinking(_ on: Bool) async {
        do {
            _ = try await app.core?.setSessionThinking(sessionId: session.id, thinking: on)
        } catch let e as CoreError {
            error = e
        } catch {}
    }
}
