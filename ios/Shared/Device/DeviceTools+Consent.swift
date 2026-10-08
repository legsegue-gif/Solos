//  The question that goes before the first time the assistant reads a
//  person's own data (calendar, reminders, contacts, location, photos, the
//  clipboard, health): what it reads goes to the model service they chose.
//  The core asks on a thread it is willing to block; the question is a system
//  alert on whatever is on screen, so it shows over a chat, the terminal or a
//  sheet alike.

import UIKit

/// The answer, passed back from the main thread.
private final class Answer: @unchecked Sendable {
    var value: Bool?
}

extension DeviceTools {
    /// `nil` when nobody can be asked just now: Solos is not on screen.
    func consent(kind: ConsentKind, capability: String) -> Bool? {
        #if DEBUG
        // A headless run has nobody to ask: SOLOS_TEST_CONSENT = allow | deny.
        if let preset = ProcessInfo.processInfo.environment["SOLOS_TEST_CONSENT"] { return preset == "allow" }
        #endif
        let (title, message) = Self.consentText(kind, capability)
        let semaphore = DispatchSemaphore(value: 0)
        let answer = Answer()
        DispatchQueue.main.async {
            guard UIApplication.shared.applicationState == .active, let top = Self.topViewController() else {
                semaphore.signal()
                return
            }
            let alert = UIAlertController(title: title, message: message, preferredStyle: .alert)
            alert.addAction(UIAlertAction(title: String(localized: "Don't Allow"), style: .cancel) { _ in
                answer.value = false
                semaphore.signal()
            })
            alert.addAction(UIAlertAction(title: String(localized: "Allow"), style: .default) { _ in
                answer.value = true
                semaphore.signal()
            })
            top.present(alert, animated: true)
        }
        // Someone who walked away is not an answer.
        if semaphore.wait(timeout: .now() + 600) == .timedOut { return nil }
        return answer.value
    }

    static func consentText(_ kind: ConsentKind, _ capability: String) -> (title: String, message: String) {
        switch kind {
        case .health:
            return (
                String(localized: "Allow access to your health data?"),
                String(localized: "The assistant wants to use your health data. What it reads becomes part of the conversation and is sent to the AI service you chose in Settings, not to Solos. Allow this only if you trust that service with health data. Solos never uses health data for advertising. You can change this in Settings > Privacy."))
        case .personal:
            let what: String = switch capability {
            case "device_calendar": String(localized: "your calendar")
            case "device_reminders": String(localized: "your reminders")
            case "device_contacts": String(localized: "your contacts")
            case "device_location": String(localized: "your location")
            case "device_photos": String(localized: "your photos")
            case "device_clipboard": String(localized: "your clipboard")
            default: String(localized: "your personal data")
            }
            return (
                String(localized: "Allow access to \(what)?"),
                String(localized: "The assistant wants to read \(what). What it reads becomes part of the conversation and is sent to the AI service you chose in Settings, not to Solos. This also covers your calendar, reminders, contacts, location, photos and clipboard. You can change this in Settings > Privacy."))
        }
    }

    @MainActor
    private static func topViewController() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let scene = scenes.first { $0.activationState == .foregroundActive } ?? scenes.first
        var top = scene?.windows.first(where: { $0.isKeyWindow })?.rootViewController ?? scene?.windows.first?.rootViewController
        while let presented = top?.presentedViewController { top = presented }
        return top
    }
}
