//  Clipboard, local notifications, opening URLs and apps, and what device this is.

import Foundation
import UIKit
import UserNotifications

extension DeviceTools {
    func clipboard(_ input: [String: Any]) throws -> [String: Any] {
        switch input["action"] as? String {
        case "read":
            let text = try onMain { UIPasteboard.general.string ?? "" }
            return ["text": text, "empty": text.isEmpty]
        case "write":
            guard let text = input["text"] as? String else { return ["error": "`text` is required."] }
            try onMain { UIPasteboard.general.string = text }
            return ["ok": true, "message": "Copied \(text.count) characters to the clipboard."]
        default:
            return ["error": "`action` must be read or write."]
        }
    }

    func notify(_ input: [String: Any]) throws -> [String: Any] {
        guard let title = input["heading"] as? String, !title.isEmpty else {
            return ["error": "`heading` is required."]
        }
        let center = UNUserNotificationCenter.current()
        let semaphore = DispatchSemaphore(value: 0)
        var granted = false
        center.requestAuthorization(options: [.alert, .sound]) { ok, _ in
            granted = ok
            semaphore.signal()
        }
        _ = semaphore.wait(timeout: .now() + Self.personDeadline)
        guard granted else { throw DeviceError.message("The user did not allow notifications.") }

        let content = UNMutableNotificationContent()
        content.title = title
        content.body = input["body"] as? String ?? ""
        content.sound = .default

        var trigger: UNNotificationTrigger?
        var when = "now"
        if let at = try DateArg.optional(input, "at") {
            let interval = max(1, at.timeIntervalSinceNow)
            trigger = UNTimeIntervalNotificationTrigger(timeInterval: interval, repeats: false)
            when = DateArg.format(at)
        }
        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: trigger)
        let done = DispatchSemaphore(value: 0)
        var failure: Error?
        center.add(request) { error in
            failure = error
            done.signal()
        }
        _ = done.wait(timeout: .now() + 10)
        if let failure { throw failure }
        return ["ok": true, "message": "Notification \"\(title)\" scheduled for \(when)."]
    }

    func open(_ input: [String: Any]) throws -> [String: Any] {
        guard let raw = input["url"] as? String, let url = URL(string: raw) else {
            return ["error": "`url` is missing or not a URL."]
        }
        let opened = try onMain { UIApplication.shared.canOpenURL(url) }
        guard opened else { return ["error": "No app can open \(raw)."] }
        try onMain { UIApplication.shared.open(url) }
        return ["ok": true, "message": "Opened \(raw)."]
    }

    func info() throws -> [String: Any] {
        let device = try onMain { () -> [String: Any] in
            UIDevice.current.isBatteryMonitoringEnabled = true
            let battery = UIDevice.current.batteryLevel
            return [
                "model": UIDevice.current.model,
                "system": "\(UIDevice.current.systemName) \(UIDevice.current.systemVersion)",
                "battery_percent": battery < 0 ? -1 : Int(battery * 100),
                "charging": UIDevice.current.batteryState == .charging
                    || UIDevice.current.batteryState == .full,
            ]
        }
        var out = device
        out["locale"] = Locale.current.identifier
        out["timezone"] = TimeZone.current.identifier
        out["local_time"] = DateArg.format(Date())
        return out
    }
}
