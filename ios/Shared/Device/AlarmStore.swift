//  AlarmStore.swift — the labels AlarmKit will not give back, and the list the
//  session screen shows.
//
//  Two problems, one file.
//
//  The first is AlarmKit's: an `Alarm` delivered by `alarmUpdates` carries its
//  schedule and its state but *not* the alert title it was scheduled with. So
//  the name the user gave an alarm cannot be recovered from the framework, and
//  a list of alarms would read "07:30" with no idea which one is the gym. The
//  reference implementation hit this too and solved it the same way: keep an
//  id → label map next to the alarms and look it up when listing.
//
//  The second is ours: a capability that leaves state behind needs somewhere
//  that state can be seen, and an AlarmKit alarm is invisible in the
//  Clock app. `AlarmStore` is what the session list observes so an alarm button
//  can appear exactly while something is pending.

import Foundation
#if canImport(AlarmKit)
import AlarmKit
#endif

/// Labels survive here; the alarms themselves live in AlarmKit.
///
/// `UserDefaults` rather than a table of its own: a person has a handful of
/// alarms, the map is read when a list is drawn and written when one is set,
/// and a database for that would be more moving parts than the problem has.
@MainActor
final class AlarmStore: ObservableObject {
    static let shared = AlarmStore()

    /// What the session list watches. Empty means the alarm button is not
    /// shown at all — an icon for something that does not exist is worse than
    /// no icon.
    @Published private(set) var alarms: [AlarmSummary] = []

    private let labelsKey = "device.alarm.labels"

    private init() {}

    // MARK: - Labels

    func label(for id: String) -> String? {
        labels()[id]
    }

    func setLabel(_ label: String, for id: String) {
        guard !label.isEmpty else { return }
        var all = labels()
        all[id] = label
        UserDefaults.standard.set(all, forKey: labelsKey)
    }

    func removeLabel(for id: String) {
        var all = labels()
        all.removeValue(forKey: id)
        UserDefaults.standard.set(all, forKey: labelsKey)
    }

    /// Drop labels for alarms that no longer exist. Alarms that fire or expire
    /// never come back through `cancel`, so without this the map only grows.
    func pruneLabels(keeping live: Set<String>) {
        let all = labels()
        let kept = all.filter { live.contains($0.key) }
        if kept.count != all.count {
            UserDefaults.standard.set(kept, forKey: labelsKey)
        }
    }

    private func labels() -> [String: String] {
        UserDefaults.standard.dictionary(forKey: labelsKey) as? [String: String] ?? [:]
    }

    // MARK: - The list the UI shows

    func replace(with summaries: [AlarmSummary]) {
        alarms = summaries.sorted { ($0.fireDate ?? .distantFuture) < ($1.fireDate ?? .distantFuture) }
        pruneLabels(keeping: Set(summaries.map(\.id)))
    }

    /// Ask AlarmKit what is pending and publish it. Safe to call when the
    /// framework is unavailable — the list simply stays empty and the button
    /// never appears.
    func refresh() {
        #if DEBUG
        // The simulator's AlarmKit refuses authorization without asking, so
        // the list is checked on screen with SOLOS_TEST_ALARMS=1 instead.
        if ProcessInfo.processInfo.environment["SOLOS_TEST_ALARMS"] == "1" {
            replace(with: [
                AlarmSummary(id: "a", label: "Standup", kind: "alarm", state: "scheduled",
                             fireDate: Date().addingTimeInterval(3_600), timeOfDay: nil, repeatDays: [], countdownSeconds: nil),
                AlarmSummary(id: "b", label: "", kind: "alarm", state: "scheduled",
                             fireDate: nil, timeOfDay: "07:30", repeatDays: ["monday", "tuesday", "wednesday", "thursday", "friday"], countdownSeconds: nil),
                AlarmSummary(id: "c", label: "Noodles", kind: "timer", state: "alerting",
                             fireDate: nil, timeOfDay: nil, repeatDays: [], countdownSeconds: 180),
            ])
            return
        }
        #endif
        #if canImport(AlarmKit)
        guard #available(iOS 26.0, *) else { return }
        Task { @MainActor in
            let summaries = await AlarmBridge.list()
            self.replace(with: summaries)
        }
        #endif
    }
}

/// One pending alarm, in the shape the list needs rather than the shape
/// AlarmKit hands over.
struct AlarmSummary: Identifiable, Equatable {
    let id: String
    let label: String
    /// `alarm` for a time of day, `timer` for a countdown.
    let kind: String
    let state: String
    /// When it will go off, for a fixed alarm.
    let fireDate: Date?
    /// `HH:mm`, for a repeating alarm — which has no single date.
    let timeOfDay: String?
    /// English day names (`monday`), for a repeating alarm.
    let repeatDays: [String]
    let countdownSeconds: Int?

    var displayTime: String {
        if let timeOfDay { return timeOfDay }
        if let fireDate {
            let f = DateFormatter()
            f.dateFormat = "HH:mm"
            return f.string(from: fireDate)
        }
        if let countdownSeconds {
            let h = countdownSeconds / 3600, m = (countdownSeconds % 3600) / 60, s = countdownSeconds % 60
            return h > 0
                ? String(format: "%d:%02d:%02d", h, m, s)
                : String(format: "%02d:%02d", m, s)
        }
        return "--:--"
    }

    var subtitle: String {
        if !repeatDays.isEmpty {
            // Stored as English day names; shown as the reader's own.
            let english = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"]
            let symbols = Calendar.current.shortWeekdaySymbols
            return repeatDays.compactMap { english.firstIndex(of: $0).map { symbols[$0] } }.joined(separator: " ")
        }
        if kind == "timer" { return String(localized: "Timer") }
        guard let fireDate else { return "" }
        let f = DateFormatter()
        f.dateStyle = .medium
        f.timeStyle = .none
        return f.string(from: fireDate)
    }
}
