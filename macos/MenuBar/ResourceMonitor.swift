import Foundation
import Combine

enum MenuIndicator: String, CaseIterable {
    case quota, cpu, memory

    var title: String {
        switch self {
        case .quota: "Agent quota"
        case .cpu: "CPU"
        case .memory: "Memory"
        }
    }
}

@MainActor
final class ResourceMonitor: NSObject, ObservableObject {
    @Published private(set) var reading = SystemReading()
    var changed: (() -> Void)?
    private var sampler = SystemSampler()
    private var timer: Timer?
    private static let interval: TimeInterval = 5

    func setEnabled(_ enabled: Bool) {
        if !enabled {
            guard let timer else { return }
            timer.invalidate(); self.timer = nil
            sampler.reset()
            publish(SystemReading())
        } else if timer == nil {
            sample()
            let timer = Timer(timeInterval: Self.interval, target: self, selector: #selector(sample), userInfo: nil, repeats: true)
            timer.tolerance = 1
            timer.fireDate = Date(timeIntervalSinceNow: Self.interval)
            RunLoop.main.add(timer, forMode: .common)
            self.timer = timer
        }
    }

    @objc private func sample() { publish(sampler.sample()) }

    private func publish(_ next: SystemReading) {
        // VM counters can move by a single page without changing anything on screen.
        guard reading.cpuPercent != next.cpuPercent || reading.memory?.percent != next.memory?.percent
                || reading.memory?.displayedUsedGiB != next.memory?.displayedUsedGiB else { return }
        reading = next
        changed?()
    }
}

extension MemoryReading {
    fileprivate var displayedUsedGiB: Double { (Double(usedBytes) * 10 / 1_073_741_824).rounded(.toNearestOrEven) / 10 }

    var amounts: String {
        String(format: "%.1f / %.0f GiB", displayedUsedGiB, Double(totalBytes) / 1_073_741_824)
    }
}
