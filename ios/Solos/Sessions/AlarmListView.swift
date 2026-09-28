import SwiftUI

/// Where an alarm the model set can be seen. An AlarmKit alarm does not
/// appear in the Clock app, so without this screen a person who asked for one
/// could not check it, cancel it, or tell why their phone went off.
struct AlarmListView: View {
    @ObservedObject private var store = AlarmStore.shared
    @Environment(\.dismiss) private var dismiss
    @State private var cancelling: AlarmSummary?

    var body: some View {
        NavigationStack {
            Group {
                if store.alarms.isEmpty {
                    // Reached after the last one is cancelled.
                    ContentUnavailableView(String(localized: "No alarms"), systemImage: "alarm")
                } else {
                    List {
                        ForEach(store.alarms) { alarm in
                            row(alarm)
                                .swipeActions(edge: .trailing) {
                                    Button(role: .destructive) { cancelling = alarm } label: {
                                        Label(String(localized: "Cancel alarm"), systemImage: "trash")
                                    }
                                }
                        }
                        Section {
                            Text("These alarms live only in Solos; the Clock app does not show them.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .listStyle(.plain)
                }
            }
            .navigationTitle(String(localized: "Alarms"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button(String(localized: "Done")) { dismiss() }
                }
            }
        }
        .onAppear { store.refresh() }
        .confirmationDialog(
            String(localized: "Cancel this alarm?"),
            isPresented: Binding(get: { cancelling != nil }, set: { if !$0 { cancelling = nil } }),
            titleVisibility: .visible
        ) {
            Button(String(localized: "Cancel alarm"), role: .destructive) {
                if let alarm = cancelling { cancel(alarm) }
                cancelling = nil
            }
            Button(String(localized: "Keep"), role: .cancel) { cancelling = nil }
        } message: {
            Text(cancelling.map { $0.label.isEmpty ? $0.displayTime : "\($0.displayTime) · \($0.label)" } ?? "")
        }
    }

    private func row(_ alarm: AlarmSummary) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text(alarm.displayTime)
                .font(.system(size: 30, weight: .light, design: .rounded))
                .monospacedDigit()
            VStack(alignment: .leading, spacing: 2) {
                if !alarm.label.isEmpty {
                    Text(alarm.label)
                }
                Text(alarm.subtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            if alarm.state == "alerting" {
                Image(systemName: "bell.badge.fill")
                    .foregroundStyle(.orange)
                    .symbolEffect(.pulse)
            } else if alarm.kind == "timer" {
                Image(systemName: "timer").foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 4)
    }

    private func cancel(_ alarm: AlarmSummary) {
        guard #available(iOS 26.0, *) else { return }
        Task {
            try? await AlarmBridge.cancel(id: alarm.id)
            store.refresh()
        }
    }
}
