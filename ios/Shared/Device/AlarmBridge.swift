//  AlarmBridge.swift — AlarmKit, which is Swift-only and iOS 26+.
//
//  Kept apart from `DeviceTools` so the availability fence lives in one place:
//  everything here is `@available(iOS 26.0, *)`, and the capability is simply
//  not registered on anything older (`DeviceTools.capabilities()`).

import Foundation

#if canImport(AlarmKit)
import AlarmKit

/// AlarmKit wants metadata conforming to its own protocol; we carry none.
@available(iOS 26.0, *)
nonisolated struct SolosAlarmMetadata: AlarmMetadata {}

@available(iOS 26.0, *)
enum AlarmBridge {

    // MARK: - Authorization

    /// Asked for before scheduling, never before listing: a person who opens
    /// the alarm screen to see an empty list should not be prompted for a
    /// permission nothing is about to use.
    static func authorize() async -> (granted: Bool, state: String) {
        let manager = AlarmManager.shared
        switch manager.authorizationState {
        case .authorized:
            return (true, "authorized")
        case .notDetermined:
            let after = try? await manager.requestAuthorization()
            return (after == .authorized, "notDetermined → \(after.map { "\($0)" } ?? "error")")
        case .denied:
            return (false, "denied")
        @unknown default:
            return (false, "unknown")
        }
    }

    // MARK: - Scheduling

    static func schedule(
        label: String,
        fireDate: Date,
        repeatMode: String
    ) async throws -> String {
        let alert = AlarmPresentation.Alert(
            title: label.isEmpty ? LocalizedStringResource("Alarm") : LocalizedStringResource(stringLiteral: label),
            stopButton: AlarmButton(text: LocalizedStringResource("Stop"), textColor: .white, systemImageName: "stop.circle")
        )
        let attributes = AlarmAttributes<SolosAlarmMetadata>(
            presentation: AlarmPresentation(alert: alert),
            tintColor: .blue
        )
        let schedule: Alarm.Schedule
        switch repeatMode {
        case "daily", "weekdays":
            let comps = Calendar.current.dateComponents([.hour, .minute], from: fireDate)
            let days: Set<Locale.Weekday> = repeatMode == "daily"
                ? [.sunday, .monday, .tuesday, .wednesday, .thursday, .friday, .saturday]
                : [.monday, .tuesday, .wednesday, .thursday, .friday]
            schedule = .relative(
                Alarm.Schedule.Relative(
                    time: Alarm.Schedule.Relative.Time(
                        hour: comps.hour ?? 0,
                        minute: comps.minute ?? 0
                    ),
                    repeats: .weekly(Array(days))
                )
            )
        default:
            schedule = .fixed(fireDate)
        }
        let alarm = try await AlarmManager.shared.schedule(
            id: UUID(),
            configuration: AlarmManager.AlarmConfiguration(schedule: schedule, attributes: attributes)
        )
        // The label has to be kept here: AlarmKit does not hand the alert
        // title back when listing, so this is the only copy.
        await MainActor.run { AlarmStore.shared.setLabel(label, for: alarm.id.uuidString) }
        return alarm.id.uuidString
    }

    static func scheduleTimer(label: String, duration: TimeInterval) async throws -> String {
        let alert = AlarmPresentation.Alert(
            title: label.isEmpty ? LocalizedStringResource("Timer") : LocalizedStringResource(stringLiteral: label),
            stopButton: AlarmButton(text: LocalizedStringResource("Done"), textColor: .green, systemImageName: "checkmark")
        )
        let attributes = AlarmAttributes<SolosAlarmMetadata>(
            presentation: AlarmPresentation(alert: alert),
            tintColor: .orange
        )
        let alarm = try await AlarmManager.shared.schedule(
            id: UUID(),
            configuration: AlarmManager.AlarmConfiguration(
                countdownDuration: Alarm.CountdownDuration(preAlert: duration, postAlert: nil),
                attributes: attributes
            )
        )
        await MainActor.run { AlarmStore.shared.setLabel(label, for: alarm.id.uuidString) }
        return alarm.id.uuidString
    }

    // MARK: - Listing and cancelling

    static func list() async -> [AlarmSummary] {
        // `alarmUpdates` is a stream; the first emission is the current state,
        // which is all a one-shot listing wants.
        for await alarms in AlarmManager.shared.alarmUpdates {
            let labels = await MainActor.run { () -> [String: String] in
                let store = AlarmStore.shared
                return Dictionary(uniqueKeysWithValues: alarms.map {
                    ($0.id.uuidString, store.label(for: $0.id.uuidString) ?? "")
                })
            }
            return alarms.map { alarm in
                var fireDate: Date?
                var timeOfDay: String?
                var repeatDays: [String] = []
                var kind = "timer"
                if let schedule = alarm.schedule {
                    kind = "alarm"
                    switch schedule {
                    case .fixed(let date):
                        fireDate = date
                    case .relative(let rel):
                        timeOfDay = String(format: "%02d:%02d", rel.time.hour, rel.time.minute)
                        if case .weekly(let days) = rel.repeats {
                            repeatDays = Self.names(days)
                        }
                    @unknown default:
                        break
                    }
                }
                let state: String
                switch alarm.state {
                case .countdown: state = "countdown"
                case .alerting: state = "alerting"
                case .scheduled: state = "scheduled"
                @unknown default: state = "unknown"
                }
                var countdown: Int?
                if let seconds = alarm.countdownDuration?.preAlert { countdown = Int(seconds) }
                return AlarmSummary(
                    id: alarm.id.uuidString,
                    label: labels[alarm.id.uuidString] ?? "",
                    kind: kind,
                    state: state,
                    fireDate: fireDate,
                    timeOfDay: timeOfDay,
                    repeatDays: repeatDays,
                    countdownSeconds: countdown
                )
            }
        }
        return []
    }

    static func cancel(id: String) async throws {
        guard let uuid = UUID(uuidString: id) else { return }
        try AlarmManager.shared.cancel(id: uuid)
        await MainActor.run { AlarmStore.shared.removeLabel(for: id) }
    }

    static func cancelAll() async throws -> Int {
        let current = await list()
        for alarm in current {
            if let uuid = UUID(uuidString: alarm.id) {
                try? AlarmManager.shared.cancel(id: uuid)
            }
            await MainActor.run { AlarmStore.shared.removeLabel(for: alarm.id) }
        }
        return current.count
    }

    /// English day names, Monday first: what the model reads, and what the
    /// list turns into the reader's own short day names.
    private static func names(_ days: [Locale.Weekday]) -> [String] {
        let order: [Locale.Weekday] = [.monday, .tuesday, .wednesday, .thursday, .friday, .saturday, .sunday]
        return order.filter { days.contains($0) }.map(\.rawValue)
    }
}
#endif
