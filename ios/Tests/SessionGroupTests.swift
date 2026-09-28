import XCTest
@testable import Solos

final class SessionGroupTests: XCTestCase {
    private func session(_ id: String, daysAgo: Double, pinned: Bool = false, now: Date) -> SessionInfo {
        let ms = Int64((now.timeIntervalSince1970 - daysAgo * 86_400) * 1000)
        return SessionInfo(id: id, title: id, model: nil, thinking: nil, createdAt: ms, updatedAt: ms, preview: nil, pinnedAt: pinned ? ms : nil)
    }

    /// Pinned first whatever its age, then today, yesterday, the week, the
    /// month and the rest; groups with nothing in them are left out.
    func testChatsFallIntoTheReferenceAppsGroups() {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let now = ISO8601DateFormatter().date(from: "2026-09-26T12:00:00Z")!
        let list = [
            session("old", daysAgo: 60, now: now),
            session("today", daysAgo: 0.1, now: now),
            session("pinned", daysAgo: 90, pinned: true, now: now),
            session("yesterday", daysAgo: 1, now: now),
            session("week", daysAgo: 4, now: now),
            session("month", daysAgo: 20, now: now),
        ]
        let groups = SessionGroup.make(list, now: now, calendar: calendar)
        XCTAssertEqual(groups.map(\.group), [.pinned, .today, .yesterday, .thisWeek, .thisMonth, .earlier])
        XCTAssertEqual(groups.map { $0.sessions.map(\.id) }, [["pinned"], ["today"], ["yesterday"], ["week"], ["month"], ["old"]])
        XCTAssertEqual(SessionGroup.make([session("a", daysAgo: 2, now: now)], now: now, calendar: calendar).map(\.group), [.thisWeek])
    }
}
