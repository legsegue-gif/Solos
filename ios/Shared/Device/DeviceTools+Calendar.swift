//  Calendar and reminders, through EventKit. Access is the system's full-access prompt, raised by the first call.

import Foundation
import EventKit

extension DeviceTools {
    func calendar(_ input: [String: Any]) throws -> [String: Any] {
        try requireAccess(to: .event, named: "the calendar")
        switch input["action"] as? String {
        case "list":
            let from = try DateArg.optional(input, "from") ?? Date()
            let to = try DateArg.optional(input, "to", end: true) ?? from.addingTimeInterval(7 * 86_400)
            let predicate = eventStore.predicateForEvents(withStart: from, end: to, calendars: nil)
            let items = eventStore.events(matching: predicate).map { event -> [String: Any] in
                [
                    "id": event.eventIdentifier ?? "",
                    "name": event.title ?? "",
                    "start": DateArg.format(event.startDate),
                    "end": DateArg.format(event.endDate),
                    "all_day": event.isAllDay,
                    "location": event.location ?? "",
                    "calendar": event.calendar?.title ?? "",
                ]
            }
            return ["items": items, "from": DateArg.format(from), "to": DateArg.format(to)]

        case "create":
            guard let title = input["name"] as? String, !title.isEmpty else {
                return ["error": "`name` is required."]
            }
            guard let start = try DateArg.optional(input, "start") else {
                return ["error": "`start` is required."]
            }
            let event = EKEvent(eventStore: eventStore)
            event.title = title
            event.startDate = start
            event.endDate = try DateArg.optional(input, "end") ?? start.addingTimeInterval(3_600)
            event.isAllDay = input["all_day"] as? Bool ?? false
            event.location = input["location"] as? String
            event.notes = input["notes"] as? String
            event.calendar = eventStore.defaultCalendarForNewEvents
            try eventStore.save(event, span: .thisEvent, commit: true)
            return [
                "ok": true,
                "id": event.eventIdentifier ?? "",
                "message": "Created \"\(title)\" at \(DateArg.format(event.startDate)).",
            ]

        case "delete":
            guard let id = input["id"] as? String,
                  let event = eventStore.event(withIdentifier: id)
            else { return ["error": "No event with that id."] }
            let title = event.title ?? ""
            try eventStore.remove(event, span: .thisEvent, commit: true)
            return ["ok": true, "message": "Deleted \"\(title)\"."]

        default:
            return ["error": "`action` must be list, create or delete."]
        }
    }

    func reminders(_ input: [String: Any]) throws -> [String: Any] {
        try requireAccess(to: .reminder, named: "reminders")
        switch input["action"] as? String {
        case "list":
            let wantCompleted = input["completed"] as? Bool ?? false
            let predicate = wantCompleted
                ? eventStore.predicateForCompletedReminders(withCompletionDateStarting: nil, ending: nil, calendars: nil)
                : eventStore.predicateForIncompleteReminders(withDueDateStarting: nil, ending: nil, calendars: nil)
            let found = try fetchReminders(predicate)
            let items = found.map { r -> [String: Any] in
                var item: [String: Any] = [
                    "id": r.calendarItemIdentifier,
                    "name": r.title ?? "",
                    "completed": r.isCompleted,
                ]
                if let due = r.dueDateComponents?.date { item["due"] = DateArg.format(due) }
                if let notes = r.notes { item["notes"] = notes }
                return item
            }
            return ["items": items]

        case "create":
            guard let title = input["name"] as? String, !title.isEmpty else {
                return ["error": "`name` is required."]
            }
            let reminder = EKReminder(eventStore: eventStore)
            reminder.title = title
            reminder.notes = input["notes"] as? String
            reminder.calendar = eventStore.defaultCalendarForNewReminders()
            if let due = try DateArg.optional(input, "due") {
                reminder.dueDateComponents = Calendar.current.dateComponents(
                    [.year, .month, .day, .hour, .minute], from: due)
                reminder.addAlarm(EKAlarm(absoluteDate: due))
            }
            try eventStore.save(reminder, commit: true)
            return ["ok": true, "id": reminder.calendarItemIdentifier, "message": "Added \"\(title)\".",
                    "due": reminder.alarms?.first?.absoluteDate.map(DateArg.format) ?? NSNull()]

        case "complete":
            guard let id = input["id"] as? String,
                  let reminder = eventStore.calendarItem(withIdentifier: id) as? EKReminder
            else { return ["error": "No reminder with that id."] }
            reminder.isCompleted = true
            try eventStore.save(reminder, commit: true)
            return ["ok": true, "message": "Completed \"\(reminder.title ?? "")\"."]

        default:
            return ["error": "`action` must be list, create or complete."]
        }
    }

    /// EventKit fetches reminders asynchronously even though events are
    /// synchronous, so this call blocks the core thread it already owns.
    func fetchReminders(_ predicate: NSPredicate) throws -> [EKReminder] {
        let semaphore = DispatchSemaphore(value: 0)
        var found: [EKReminder] = []
        eventStore.fetchReminders(matching: predicate) { reminders in
            found = reminders ?? []
            semaphore.signal()
        }
        if semaphore.wait(timeout: .now() + 20) == .timedOut {
            throw DeviceError.message("Reading reminders timed out.")
        }
        return found
    }

    func requireAccess(to entity: EKEntityType, named: String) throws {
        let status = EKEventStore.authorizationStatus(for: entity)
        if #available(iOS 17.0, *) {
            if status == .fullAccess { return }
        }
        if status == .authorized { return }
        if status == .denied || status == .restricted {
            throw DeviceError.message("Access to \(named) is denied; it can be allowed in Settings.")
        }
        let semaphore = DispatchSemaphore(value: 0)
        var granted = false
        let handler: EKEventStoreRequestAccessCompletionHandler = { ok, _ in
            granted = ok
            semaphore.signal()
        }
        if #available(iOS 17.0, *) {
            switch entity {
            case .event: eventStore.requestFullAccessToEvents(completion: handler)
            case .reminder: eventStore.requestFullAccessToReminders(completion: handler)
            @unknown default: eventStore.requestAccess(to: entity, completion: handler)
            }
        } else {
            eventStore.requestAccess(to: entity, completion: handler)
        }
        _ = semaphore.wait(timeout: .now() + Self.personDeadline)
        if !granted { throw DeviceError.message("The user did not allow access to \(named).") }
    }
}
