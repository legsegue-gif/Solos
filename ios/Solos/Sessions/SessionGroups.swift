import Foundation

/// The chat list's sections, as the reference app has them: pinned chats
/// first, then by when each was last active.
enum SessionGroup: Hashable {
    case pinned, today, yesterday, thisWeek, thisMonth, earlier

    var title: String {
        switch self {
        case .pinned: String(localized: "Pinned")
        case .today: String(localized: "Today")
        case .yesterday: String(localized: "Yesterday")
        case .thisWeek: String(localized: "This Week")
        case .thisMonth: String(localized: "This Month")
        case .earlier: String(localized: "Earlier")
        }
    }

    /// Non-empty groups in order, each newest first. Pinned chats are
    /// ordered by activity, then by when they were pinned.
    static func make(_ sessions: [SessionInfo], now: Date = .now, calendar: Calendar = .current) -> [(group: SessionGroup, sessions: [SessionInfo])] {
        let weekAgo = calendar.date(byAdding: .day, value: -7, to: now) ?? now
        let monthAgo = calendar.date(byAdding: .month, value: -1, to: now) ?? now
        var buckets: [SessionGroup: [SessionInfo]] = [:]
        for s in sessions {
            let group: SessionGroup
            let date = s.updatedDate
            if s.pinnedAt != nil {
                group = .pinned
            } else if calendar.isDate(date, inSameDayAs: now) {
                group = .today
            } else if let yesterday = calendar.date(byAdding: .day, value: -1, to: now), calendar.isDate(date, inSameDayAs: yesterday) {
                group = .yesterday
            } else if date > weekAgo {
                group = .thisWeek
            } else if date > monthAgo {
                group = .thisMonth
            } else {
                group = .earlier
            }
            buckets[group, default: []].append(s)
        }
        let order: [SessionGroup] = [.pinned, .today, .yesterday, .thisWeek, .thisMonth, .earlier]
        return order.compactMap { g in
            guard var list = buckets[g], !list.isEmpty else { return nil }
            list.sort { a, b in
                a.updatedAt != b.updatedAt ? a.updatedAt > b.updatedAt : (a.pinnedAt ?? 0) > (b.pinnedAt ?? 0)
            }
            return (g, list)
        }
    }
}

extension SessionInfo {
    var updatedDate: Date { Date(timeIntervalSince1970: TimeInterval(updatedAt) / 1000) }

    /// When it was last active, as a row shows it: minutes or hours ago
    /// today, then "Yesterday", the weekday within a week, the date after.
    func activityLabel(now: Date = .now, calendar: Calendar = .current) -> String {
        let date = updatedDate
        if calendar.isDate(date, inSameDayAs: now) {
            let f = RelativeDateTimeFormatter()
            f.unitsStyle = .short
            return f.localizedString(for: date, relativeTo: now)
        }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now), calendar.isDate(date, inSameDayAs: yesterday) {
            return String(localized: "Yesterday")
        }
        if let weekAgo = calendar.date(byAdding: .day, value: -7, to: now), date > weekAgo {
            return date.formatted(.dateTime.weekday(.wide))
        }
        return date.formatted(date: .abbreviated, time: .omitted)
    }
}
