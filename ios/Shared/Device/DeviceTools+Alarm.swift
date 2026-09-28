//  Alarms and timers through AlarmKit (iOS 26), listed in the app because nothing else shows them.

import Foundation
import AlarmKit

extension DeviceTools {
    /// Alarms ring; notifications do not. That is the whole reason this is a
    /// separate capability from `device_notify`, and the reason it is worth the
    /// screen it needs: an AlarmKit alarm is invisible in the Clock
    /// app, so this app has to be where it can be seen.
    func alarm(_ input: [String: Any]) throws -> [String: Any] {
        guard #available(iOS 26.0, *) else {
            return ["error": "Alarms need iOS 26 or later (AlarmKit)."]
        }
        let action = input["action"] as? String ?? "list"
        let label = (input["label"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

        switch action {
        case "set":
            guard let when = try DateArg.optional(input, "time") else {
                return ["error": "`time` is required."]
            }
            let auth = awaitAlarmAuth()
            guard auth.granted else {
                // The state itself, because "denied" and "the prompt never
                // appeared" call for different responses and look identical
                // from here otherwise.
                return ["error": "Not allowed to set alarms (AlarmKit authorisation: \(auth.state))."]
            }
            let repeatMode = input["repeat"] as? String ?? "none"
            let id = try awaitAlarm { try await AlarmBridge.schedule(label: label, fireDate: when, repeatMode: repeatMode) }
            refreshAlarmList()
            return [
                "ok": true, "id": id, "label": label,
                "fires_at": DateArg.format(when),
                "repeat": repeatMode,
                // Said in the result as well as in the tool description,
                // because this is the part a user will otherwise go looking
                // for in the Clock app and not find.
                "where_to_see_it": "The alarm button at the top of the chat list; alarms set here do not appear in the Clock app.",
            ]

        case "timer":
            guard let seconds = Self.parseDuration(input["duration"]) else {
                return ["error": "`duration` is missing or unreadable (300, 5m and 1h30m all work)."]
            }
            let auth = awaitAlarmAuth()
            guard auth.granted else {
                return ["error": "Not allowed to set alarms (AlarmKit authorisation: \(auth.state))."]
            }
            let id = try awaitAlarm { try await AlarmBridge.scheduleTimer(label: label, duration: seconds) }
            refreshAlarmList()
            return [
                "ok": true, "id": id, "label": label,
                "duration_s": Int(seconds),
                "fires_at": DateArg.format(Date().addingTimeInterval(seconds)),
                "where_to_see_it": "The alarm button at the top of the chat list; alarms set here do not appear in the Clock app.",
            ]

        case "list":
            let items = try awaitAlarm { await AlarmBridge.list() }
            refreshAlarmList()
            return ["items": items.map { summary in
                var row: [String: Any] = [
                    "id": summary.id,
                    "kind": summary.kind,
                    "state": summary.state,
                    "time": summary.displayTime,
                ]
                if !summary.label.isEmpty { row["label"] = summary.label }
                if let fire = summary.fireDate { row["fires_at"] = DateArg.format(fire) }
                if !summary.repeatDays.isEmpty { row["repeat_days"] = summary.repeatDays }
                return row
            }]

        case "cancel":
            if input["all"] as? Bool == true {
                let count = try awaitAlarm { try await AlarmBridge.cancelAll() }
                refreshAlarmList()
                return ["ok": true, "cancelled": count]
            }
            guard let id = input["id"] as? String else { return ["error": "`id` is required (or pass all: true)."] }
            try awaitAlarm { try await AlarmBridge.cancel(id: id) }
            refreshAlarmList()
            return ["ok": true, "cancelled": 1, "id": id]

        default:
            return ["error": "Unknown action: \(action)."]
        }
    }

    /// `300`, `5m`, `1h30m`. Shorthand because a model asked for "five
    /// minutes" will write `5m` before it writes `300`.
    static func parseDuration(_ value: Any?) -> TimeInterval? {
        guard let text = (value as? String) ?? (value as? Int).map(String.init) else { return nil }
        if let plain = Double(text), plain > 0 { return plain }
        var total: TimeInterval = 0
        var number = ""
        var sawUnit = false
        for ch in text.lowercased() {
            if ch.isNumber { number.append(ch); continue }
            guard let n = Double(number) else { continue }
            switch ch {
            case "h": total += n * 3600; sawUnit = true
            case "m": total += n * 60; sawUnit = true
            case "s": total += n; sawUnit = true
            default: break
            }
            number = ""
        }
        return sawUnit && total > 0 ? total : nil
    }

    /// AlarmKit is async and this bridge is not: the core calls every device
    /// tool on a thread it is willing to block. Same shape as the MapKit
    /// waits above, for the same reason.
    func awaitAlarm<T>(_ work: @escaping @Sendable () async throws -> T) throws -> T {
        let handoff = Handoff<T>()
        Task {
            do { handoff.finish(.success(try await work())) } catch { handoff.finish(.failure(error)) }
        }
        guard let result = handoff.wait(seconds: 60) else {
            throw DeviceError.message("The alarm operation timed out.")
        }
        return try result.get()
    }

    /// The published list the session screen watches lives on the main actor;
    /// device tools run on a core thread. Hopping rather than awaiting: the
    /// caller has its answer already and the button can appear a moment later.
    func refreshAlarmList() {
        DispatchQueue.main.async { AlarmStore.shared.refresh() }
    }

    @available(iOS 26.0, *)
    func awaitAlarmAuth() -> (granted: Bool, state: String) {
        (try? awaitAlarm { await AlarmBridge.authorize() }) ?? (false, "timed out")
    }
}
