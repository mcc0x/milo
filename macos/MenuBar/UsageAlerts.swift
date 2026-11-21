import Combine
import Foundation
import UserNotifications

enum UsageAlertThreshold: Int, CaseIterable {
    case off = 0, ten = 10, twenty = 20, thirty = 30

    var title: String { self == .off ? "Off" : "\(rawValue)% remaining" }
}

struct UsageAlertNotice: Equatable {
    let providerID: String
    let providerName: String
    let windowLabel: String
    let remaining: Int
    let resetAt: Int
}

private struct UsageAlertTracker {
    struct WindowID: Hashable {
        let providerID: String
        let label: String
        let resetAt: Int
    }

    struct Crossing {
        let id: WindowID
        let streak: UInt64
    }

    private struct Reading {
        let providerName: String
        let remaining: Int
        let staleAt: Int
        let streak: UInt64
    }

    private static let historyKey = "usageAlertResets"
    private let defaults: UserDefaults
    private var posted: [String: [Int]]
    private var last: [WindowID: Reading] = [:]
    private var frame: UInt64 = 0

    init(defaults: UserDefaults) {
        self.defaults = defaults
        posted = defaults.dictionary(forKey: Self.historyKey) as? [String: [Int]] ?? [:]
    }

    mutating func clearBaseline() { last.removeAll() }

    mutating func observe(_ snapshot: UsageSnapshot, threshold: UsageAlertThreshold, now: TimeInterval) -> [Crossing] {
        frame &+= 1
        var next: [WindowID: Reading] = [:]
        var crossings: [Crossing] = []
        // The feed omits hidden providers. Missing or expired windows break the streak.
        for provider in snapshot.providers {
            for window in provider.windows {
                guard window.fresh, let staleAt = window.staleAt, Double(staleAt) > now,
                      let resetAt = window.resetAt, Double(resetAt) > now else { continue }
                let id = WindowID(providerID: provider.id, label: window.label, resetAt: resetAt)
                let before = last[id].flatMap { Double($0.staleAt) > now ? $0 : nil }
                let streak = before?.streak ?? frame
                next[id] = Reading(providerName: provider.name, remaining: window.remaining, staleAt: staleAt, streak: streak)
                if let before, before.remaining > threshold.rawValue, window.remaining <= threshold.rawValue,
                   posted[provider.id]?.contains(resetAt) != true {
                    crossings.append(Crossing(id: id, streak: streak))
                }
            }
        }
        last = next
        return crossings
    }

    mutating func reserve(_ crossing: Crossing, threshold: UsageAlertThreshold, now: TimeInterval) -> UsageAlertNotice? {
        let id = crossing.id
        guard let reading = last[id], reading.streak == crossing.streak, reading.remaining <= threshold.rawValue,
              Double(reading.staleAt) > now, Double(id.resetAt) > now,
              posted[id.providerID]?.contains(id.resetAt) != true else { return nil }
        // Record before handing the immediate request to macOS, so a relaunch
        // while it is in flight cannot submit a second notification for the reset.
        posted = posted.compactMapValues { resets in
            let current = resets.filter { Double($0) > now }
            return current.isEmpty ? nil : current
        }
        posted[id.providerID, default: []].append(id.resetAt)
        defaults.set(posted, forKey: Self.historyKey)
        return UsageAlertNotice(providerID: id.providerID, providerName: reading.providerName,
                                windowLabel: id.label, remaining: reading.remaining, resetAt: id.resetAt)
    }

    mutating func release(_ rejected: [UsageAlertNotice]) {
        guard !rejected.isEmpty else { return }
        for notice in rejected {
            posted[notice.providerID]?.removeAll { $0 == notice.resetAt }
            if posted[notice.providerID]?.isEmpty == true { posted.removeValue(forKey: notice.providerID) }
        }
        defaults.set(posted, forKey: Self.historyKey)
    }
}

enum UsageAlertPermission: Equatable {
    case notDetermined, blocked, available, failed

    var issue: String? {
        switch self {
        case .notDetermined: "Notification permission is required. Choose Off, then choose a threshold to enable it."
        case .blocked: "Allow Milo notifications in System Settings to receive low-quota alerts."
        case .failed: "Milo couldn’t confirm notification permission."
        case .available: nil
        }
    }
}

struct UsageAlertDelivery {
    enum Submission: Equatable {
        case accepted, rejected, unconfirmed

        var issue: String? {
            switch self {
            case .accepted: nil
            case .rejected: "Milo couldn’t send a low-quota alert."
            case .unconfirmed: "Milo couldn’t confirm that a low-quota alert was sent."
            }
        }
    }

    let permission: @MainActor () async -> UsageAlertPermission
    let requestPermission: @MainActor () async throws -> Bool
    let submit: @MainActor (UsageAlertNotice) async -> Submission

    static let system = Self(permission: {
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        switch settings.authorizationStatus {
        case .notDetermined: return .notDetermined
        case .denied: return .blocked
        case .authorized, .provisional:
            return settings.alertSetting == .enabled || settings.notificationCenterSetting == .enabled ? .available : .blocked
        @unknown default: return .failed
        }
    }, requestPermission: {
        try await UNUserNotificationCenter.current().requestAuthorization(options: .alert)
    }, submit: { notice in
        let content = UNMutableNotificationContent()
        content.title = "\(notice.providerName) quota is low"
        content.body = "\(notice.windowLabel): \(notice.remaining)% remaining."
        let request = UNNotificationRequest(identifier: "quota.\(notice.providerID).\(notice.resetAt)", content: content, trigger: nil)
        do {
            try await UNUserNotificationCenter.current().add(request)
            return .accepted
        } catch {
            // Only an explicit notification service rejection establishes that
            // nothing was queued; do not rearm after an uncertain submission.
            return (error as NSError).domain == UNErrorDomain ? .rejected : .unconfirmed
        }
    })
}

@MainActor
final class UsageAlerts: NSObject, ObservableObject, UNUserNotificationCenterDelegate {
    private static let thresholdKey = "usageAlertThreshold"
    private let defaults: UserDefaults
    private let delivery: UsageAlertDelivery
    private let clock: () -> TimeInterval
    private var tracker: UsageAlertTracker
    private var permissionCheck: Task<Void, Never>?
    private var canPrompt = false
    private var preferenceRevision: UInt64 = 0

    @Published private(set) var threshold: UsageAlertThreshold
    @Published private var permission: UsageAlertPermission?
    @Published private var lastSubmission: UsageAlertDelivery.Submission?

    var issue: String? {
        guard threshold != .off else { return nil }
        return permission?.issue ?? lastSubmission?.issue
    }

    init(defaults: UserDefaults = .standard, delivery: UsageAlertDelivery = .system,
         clock: @escaping () -> TimeInterval = { Date.now.timeIntervalSince1970 }) {
        self.defaults = defaults
        self.delivery = delivery
        self.clock = clock
        tracker = UsageAlertTracker(defaults: defaults)
        threshold = UsageAlertThreshold(rawValue: defaults.integer(forKey: Self.thresholdKey)) ?? .off
        super.init()
    }

    func start() {
        UNUserNotificationCenter.current().delegate = self
        refreshPermission()
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                           willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        [.banner, .list]
    }

    @discardableResult
    func select(_ threshold: UsageAlertThreshold) -> Task<Void, Never>? {
        guard self.threshold != threshold else { return nil }
        let optingIn = self.threshold == .off
        self.threshold = threshold
        defaults.set(threshold.rawValue, forKey: Self.thresholdKey)
        preferenceRevision &+= 1
        clearBaseline()
        if threshold == .off {
            canPrompt = false
            permission = nil
            lastSubmission = nil
            return nil
        }
        if optingIn { canPrompt = true }
        return refreshPermission()
    }

    @discardableResult
    func refreshPermission() -> Task<Void, Never>? {
        guard threshold != .off else { return nil }
        if let permissionCheck { return permissionCheck }
        let task = Task { [weak self] in
            guard let self else { return }
            defer { self.permissionCheck = nil; self.canPrompt = false }
            var permission = await self.delivery.permission()
            guard self.threshold != .off else { return }
            if self.canPrompt, permission == .notDetermined {
                do {
                    if try await self.delivery.requestPermission() {
                        permission = await self.delivery.permission()
                    } else {
                        permission = .blocked
                    }
                } catch {
                    permission = .failed
                }
            }
            guard self.threshold != .off else { return }
            if permission != self.permission { self.permission = permission }
        }
        permissionCheck = task
        return task
    }

    func clearBaseline() { tracker.clearBaseline() }

    @discardableResult
    func observe(_ snapshot: UsageSnapshot) -> Task<Void, Never>? {
        guard threshold != .off else { return nil }
        let crossings = tracker.observe(snapshot, threshold: threshold, now: clock())
        guard !crossings.isEmpty else { return nil }
        let revision = preferenceRevision
        return Task { [weak self] in
            guard let self, revision == self.preferenceRevision else { return }
            await self.refreshPermission()?.value
            guard self.permission == .available else { return }
            // Keep each reset reserved until this batch finishes, so another
            // crossing in the same snapshot cannot retry an explicit rejection.
            var rejected: [UsageAlertNotice] = []
            defer { self.tracker.release(rejected) }
            for crossing in crossings {
                guard revision == self.preferenceRevision else { return }
                guard let notice = self.tracker.reserve(crossing, threshold: self.threshold, now: self.clock()) else { continue }
                let result = await self.delivery.submit(notice)
                if result == .rejected { rejected.append(notice) }
                if revision == self.preferenceRevision, result != self.lastSubmission { self.lastSubmission = result }
            }
        }
    }
}
