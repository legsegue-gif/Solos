//  The platform half of the device tools. The core owns the schemas and
//  calls `call(capability:inputJson:)` on a thread it is willing to block;
//  each capability lives in its own `DeviceTools+<Name>.swift`. Anything that
//  must run on the main thread hops there and waits, with a deadline.
//  Permission prompts are the system's own, raised by a capability's first call.

import EventKit
import Foundation
import HealthKit

final class DeviceTools: DeviceHost, @unchecked Sendable {
    let eventStore = EKEventStore()
    let locationProvider = LocationProvider()
    let healthStore = HKHealthStore()

    /// Only what this device can do: a capability it lacks is never
    /// advertised, rather than advertised and always failing.
    func capabilities() -> [String] {
        var names = [
            "device_calendar", "device_reminders", "device_contacts", "device_location",
            "device_photos", "device_clipboard", "device_notify", "device_media",
            "device_weather", "device_maps", "device_vision", "device_nlp",
            "device_open", "device_info",
        ]
        if #available(iOS 26.0, *) { names.append("device_alarm") }
        if HKHealthStore.isHealthDataAvailable() { names.append("device_health") }
        return names
    }

    func call(capability: String, inputJson: String) -> String {
        let input = (try? JSONSerialization.jsonObject(with: Data(inputJson.utf8))) as? [String: Any] ?? [:]
        let result: [String: Any]
        do {
            switch capability {
            case "device_calendar": result = try calendar(input)
            case "device_reminders": result = try reminders(input)
            case "device_contacts": result = try contacts(input)
            case "device_location": result = try location()
            case "device_photos": result = try photos(input)
            case "device_clipboard": result = try clipboard(input)
            case "device_notify": result = try notify(input)
            case "device_alarm": result = try alarm(input)
            case "device_media": result = try media(input)
            case "device_health": result = try health(input)
            case "device_weather": result = try weather(input)
            case "device_maps": result = try maps(input)
            case "device_vision": result = try vision(input)
            case "device_nlp": result = try nlp(input)
            case "device_open": result = try open(input)
            case "device_info": result = try info()
            default: result = ["error": "Unknown capability: \(capability)."]
            }
        } catch {
            result = ["error": "\(error.localizedDescription)"]
        }
        let data = (try? JSONSerialization.data(withJSONObject: result))
            ?? Data(#"{"error":"The result could not be serialised."}"#.utf8)
        return String(decoding: data, as: UTF8.self)
    }

    /// A number the model may have sent as a number or as a string. Schemas say
    /// `number`, but endpoints differ on whether they honour that.
    func numberArg(_ value: Any?) -> Double? {
        if let d = value as? Double { return d }
        if let i = value as? Int { return Double(i) }
        if let s = value as? String { return Double(s) }
        return nil
    }

    /// How long a permission prompt is waited on. A person is deciding, and
    /// Health's takes two screens; one minute ran out while it was being
    /// answered. Stopping the turn ends the wait at once (the core stops
    /// waiting for the call), so a long deadline keeps no one stuck.
    static let personDeadline: TimeInterval = 300

    /// Run on the main thread and wait — but not for ever.
    ///
    /// The old version was `DispatchQueue.main.sync`, on the reasoning that
    /// these calls always arrive on a core thread and never on main. True, and
    /// beside the point: the main thread may itself be inside a core call at
    /// that moment, waiting for the very turn this tool belongs to. Each side
    /// then holds what the other needs and the app is frozen with no way back
    /// — which is what a hang on `__psynch_cvwait` looks like from Xcode.
    ///
    /// A deadline turns that into a failed tool call: the turn reports an
    /// error, the engine moves on, and the main thread is released.
    func onMain<T>(_ work: @escaping () -> T) throws -> T {
        if Thread.isMainThread { return work() }
        let handoff = Handoff<T>()
        // Handed to the main thread and not used here again.
        nonisolated(unsafe) let work = work
        DispatchQueue.main.async { handoff.finish(.success(work())) }
        guard case .success(let value)? = handoff.wait(seconds: Self.mainThreadDeadline) else {
            NSLog("[DeviceTools] waited \(Int(Self.mainThreadDeadline))s for the main thread and gave up")
            throw DeviceError.message("The device call could not get the main thread (something else is holding the screen) and gave up.")
        }
        return value
    }

    /// Long enough for anything the main thread legitimately does between
    /// frames, short enough that a person notices a stall rather than a hang.
    static let mainThreadDeadline: TimeInterval = 5
}

/// A result handed from a callback or task to the thread blocked waiting for
/// it. The bridge is synchronous — the core calls it on a thread it is
/// willing to block — while the frameworks behind it answer asynchronously.
final class Handoff<T>: @unchecked Sendable {
    let lock = NSLock()
    let done = DispatchSemaphore(value: 0)
    var result: Result<T, Error>?

    func finish(_ result: Result<T, Error>) {
        lock.withLock { if self.result == nil { self.result = result } }
        done.signal()
    }

    /// `nil` when the deadline came first.
    func wait(seconds: Double) -> Result<T, Error>? {
        guard done.wait(timeout: .now() + seconds) == .success else { return nil }
        return lock.withLock { result }
    }
}

enum DeviceError: LocalizedError {
    case message(String)
    var errorDescription: String? {
        switch self { case .message(let m): return m }
    }
}

/// The date forms the schemas promise: ISO 8601, or the handful of relative
/// words a person would actually say.
enum DateArg {
    /// An optional time argument: absent is `nil`, but one that was given and
    /// cannot be read is an error. Dropping it instead saved a reminder with
    /// no time while the model told the user the time it had asked for.
    static func optional(_ input: [String: Any], _ key: String, end: Bool = false) throws -> Date? {
        let raw = input[key]
        if raw == nil || raw is NSNull { return nil }
        if let text = raw as? String, text.trimmingCharacters(in: .whitespaces).isEmpty { return nil }
        if let date = end ? parseEnd(raw) : parse(raw) { return date }
        throw DeviceError.message("`\(key)` is \(raw.map { "\($0)" } ?? ""), which is not a time. Use ISO 8601 (2026-09-17T14:00:00+08:00) or today, tomorrow, `tomorrow 09:00`, `today 2pm`, +3d, +2h.")
    }

    /// A day word used as the end of a range means the end of that day.
    /// `from: tomorrow, to: tomorrow` is how anyone would ask for tomorrow,
    /// and taking both as midnight makes the window zero seconds wide.
    static func parseEnd(_ raw: Any?) -> Date? {
        guard let date = parse(raw) else { return nil }
        let text = (raw as? String)?.trimmingCharacters(in: .whitespaces).lowercased() ?? ""
        let bareDay = ["today", "tomorrow", "yesterday"].contains(text)
        guard bareDay else { return date }
        let calendar = Calendar.current
        return calendar.date(byAdding: DateComponents(day: 1, second: -1), to: date) ?? date
    }

    static func parse(_ raw: Any?) -> Date? {
        guard let text = (raw as? String)?.trimmingCharacters(in: .whitespaces), !text.isEmpty else {
            return nil
        }
        let now = Date()
        let calendar = Calendar.current
        // A day word, optionally with a clock time: "tomorrow", "tomorrow
        // 09:00", "today 14:30". The combination is what a model reaches for
        // once it has been told both forms exist, and rejecting it costs a
        // whole round trip to learn nothing.
        let words = text.lowercased().split(separator: " ", maxSplits: 1).map(String.init)
        if let day = dayStart(words.first ?? "", now: now, calendar: calendar) {
            guard words.count > 1 else { return day }
            if let clock = clock(words[1], on: day, calendar: calendar) { return clock }
            return nil
        }
        if let relative = relative(text, from: now) { return relative }

        let withFraction = ISO8601DateFormatter()
        withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = withFraction.date(from: text) { return d }
        if let d = ISO8601DateFormatter().date(from: text) { return d }

        // A bare clock time means the next time it is that time — which is
        // what "set an alarm for 07:30" means to everybody, and what the
        // alarm tool's description promises. Without this the promise was a
        // lie: `--time 07:30` came back as "cannot read this time".
        if let next = nextOccurrence(of: text, from: now, calendar: calendar) { return next }

        // A bare local date or date-time, which is what a model usually writes.
        for format in ["yyyy-MM-dd HH:mm", "yyyy-MM-dd'T'HH:mm", "yyyy-MM-dd"] {
            let f = DateFormatter()
            f.locale = Locale(identifier: "en_US_POSIX")
            f.timeZone = .current
            f.dateFormat = format
            if let d = f.date(from: text) { return d }
        }
        return nil
    }

    /// `07:30`, `7:30`, `2pm`, `9 am` → the next time the clock reads that.
    /// Today if it has not happened yet, tomorrow if it has.
    static func nextOccurrence(of text: String, from now: Date, calendar: Calendar) -> Date? {
        let today = calendar.startOfDay(for: now)
        guard let candidate = clock(text, on: today, calendar: calendar) else { return nil }
        if candidate > now { return candidate }
        return calendar.date(byAdding: .day, value: 1, to: candidate)
    }

    /// Midnight of the day a word names, or nil if it names no day.
    static func dayStart(_ word: String, now: Date, calendar: Calendar) -> Date? {
        switch word {
        case "now": return now
        case "today": return calendar.startOfDay(for: now)
        case "tomorrow": return calendar.date(byAdding: .day, value: 1, to: calendar.startOfDay(for: now))
        case "yesterday": return calendar.date(byAdding: .day, value: -1, to: calendar.startOfDay(for: now))
        default: return nil
        }
    }

    /// `09:00`, `9:00`, `14.30`, `9am`, `9pm` on a given day.
    static func clock(_ text: String, on day: Date, calendar: Calendar) -> Date? {
        var body = text.trimmingCharacters(in: .whitespaces)
        var afternoon = false
        var halfDay = false
        for suffix in ["am", "a.m.", "pm", "p.m."] where body.hasSuffix(suffix) {
            halfDay = true
            afternoon = suffix.hasPrefix("p")
            body = String(body.dropLast(suffix.count)).trimmingCharacters(in: .whitespaces)
            break
        }
        let parts = body.split(whereSeparator: { $0 == ":" || $0 == "." }).map(String.init)
        guard var hour = Int(parts.first ?? ""), (0...23).contains(hour) else { return nil }
        let minute = parts.count > 1 ? Int(parts[1]) ?? 0 : 0
        guard (0...59).contains(minute) else { return nil }
        if halfDay {
            if afternoon, hour < 12 { hour += 12 }
            if !afternoon, hour == 12 { hour = 0 }
        }
        return calendar.date(bySettingHour: hour, minute: minute, second: 0, of: day)
    }

    /// `+3d`, `-2h`, `+90m`, `+1w`.
    static func relative(_ text: String, from now: Date) -> Date? {
        guard let unit = text.last, "dhmw".contains(unit) else { return nil }
        let body = text.dropLast()
        guard let value = Int(body) else { return nil }
        let seconds: TimeInterval
        switch unit {
        case "m": seconds = 60
        case "h": seconds = 3_600
        case "d": seconds = 86_400
        case "w": seconds = 604_800
        default: return nil
        }
        return now.addingTimeInterval(Double(value) * seconds)
    }

    static func format(_ date: Date?) -> String {
        guard let date else { return "" }
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        f.timeZone = .current
        return f.string(from: date)
    }
}

extension Comparable {
    /// Keeps a model-supplied count inside what this tool will actually do.
    /// Clamping rather than rejecting: an out-of-range limit is a preference,
    /// not a mistake worth failing the call over.
    func clamped(to range: ClosedRange<Self>) -> Self {
        min(max(self, range.lowerBound), range.upperBound)
    }
}
