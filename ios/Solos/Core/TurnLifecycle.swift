import UIKit
import UserNotifications

/// Keeps a running turn alive when the app goes to the background, and
/// says when it finished there: a local notification titled "✅ <chat>"
/// (or "❌") with the start of the reply, as the reference app does.
/// Tapping it opens the chat.
@MainActor
final class TurnLifecycle: NSObject, UNUserNotificationCenterDelegate {
    /// Asked to open a chat (a notification was tapped).
    var openSession: ((String) -> Void)?

    private var tasks: [String: UIBackgroundTaskIdentifier] = [:]
    private var lastReply: [String: String] = [:]
    private var askedPermission = false

    override init() {
        super.init()
        UNUserNotificationCenter.current().delegate = self
    }

    func handle(_ event: Event, title: @autoclosure () -> String?) {
        switch event.kind {
        case .turnStarted:
            askPermissionOnce()
            lastReply[event.sessionId] = nil
            guard tasks[event.sessionId] == nil else { return }
            let id = event.sessionId
            tasks[id] = UIApplication.shared.beginBackgroundTask(withName: "turn") { [weak self] in
                // Out of time: the system suspends the app. The turn is cut
                // off and offered to continue when the chat is next open.
                MainActor.assumeIsolated { self?.end(id) }
            }
        case .messageCommitted(let m) where m.role == .assistant:
            let text = m.text.trimmingCharacters(in: .whitespacesAndNewlines)
            if !text.isEmpty { lastReply[event.sessionId] = text }
        case .turnFinished(_, let outcome):
            if UIApplication.shared.applicationState != .active {
                notify(event.sessionId, title: title() ?? String(localized: "New chat"), outcome: outcome)
            }
            end(event.sessionId)
        default:
            break
        }
    }

    private func end(_ sessionId: String) {
        if let task = tasks.removeValue(forKey: sessionId) {
            UIApplication.shared.endBackgroundTask(task)
        }
    }

    /// Asked while the app is in front, at the first turn: a background app
    /// cannot show the permission prompt.
    private func askPermissionOnce() {
        guard !askedPermission else { return }
        askedPermission = true
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }

    private func notify(_ sessionId: String, title: String, outcome: TurnOutcome) {
        let content = UNMutableNotificationContent()
        switch outcome {
        case .completed:
            content.title = "✅ \(title)"
            content.body = String(MarkdownModel.plainText(lastReply[sessionId] ?? "").prefix(200))
        case .failed(let error):
            content.title = "❌ \(title)"
            content.body = error.message
        case .cancelled:
            return
        }
        content.sound = .default
        content.userInfo = ["sessionId": sessionId]
        UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: "turn-\(sessionId)", content: content, trigger: nil))
        lastReply[sessionId] = nil
    }

    /// The completion-handler form, called back on the main thread: the
    /// async form finished on a background executor, and UIKit aborts when
    /// the system's completion runs off the main thread.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping @Sendable () -> Void
    ) {
        let id = response.notification.request.content.userInfo["sessionId"] as? String
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                if let id { self.openSession?(id) }
            }
            completionHandler()
        }
    }
}
