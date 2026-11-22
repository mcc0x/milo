import Darwin
import Foundation

struct SystemReading: Equatable {
    var cpuPercent: Int? = nil
    var memory: MemoryReading? = nil
}

struct MemoryReading: Equatable {
    let usedBytes: UInt64
    let totalBytes: UInt64
    let percent: Int

    // Resident anonymous pages less reclaimable purgeable pages, plus wired
    // pages and the physical pages occupied by the compressor (not its contents
    // at uncompressed size). This excludes file cache, free pages and swap.
    static func fromPages(
        anonymous: UInt32, purgeable: UInt32, wired: UInt32, compressed: UInt32,
        pageSize: UInt64, totalBytes: UInt64
    ) -> MemoryReading? {
        guard purgeable <= anonymous, pageSize > 0, pageSize <= totalBytes else { return nil }
        let usedPages = UInt64(anonymous - purgeable) + UInt64(wired) + UInt64(compressed)
        let (usedBytes, overflow) = usedPages.multipliedReportingOverflow(by: pageSize)
        guard !overflow, usedBytes <= totalBytes else { return nil }
        return MemoryReading(usedBytes: usedBytes, totalBytes: totalBytes, percent: percentage(usedBytes, of: totalBytes))
    }
}

struct SystemSampler {
    struct CPUTicks {
        let user: UInt32
        let system: UInt32
        let idle: UInt32
        let nice: UInt32

        fileprivate func busyPercent(since old: CPUTicks) -> Int? {
            // A wrapped counter and a reset both look like a decrease. Skip
            // the interval and rebase instead of interpreting a reset as work.
            guard user >= old.user, system >= old.system, idle >= old.idle, nice >= old.nice else { return nil }
            let busy = UInt64(user - old.user) + UInt64(system - old.system) + UInt64(nice - old.nice)
            let total = busy + UInt64(idle - old.idle)
            return total > 0 ? percentage(busy, of: total) : nil
        }
    }

    private static let physicalMemory = ProcessInfo.processInfo.physicalMemory
    // The fields we need are in revision 1, which ends just before swapped_count.
    private static let vmWordCount = MemoryLayout<vm_statistics64_data_t>.offset(of: \.swapped_count)
        .map { $0 / MemoryLayout<integer_t>.size }
    private var previousCPU: (ticks: CPUTicks, at: ContinuousClock.Instant)?

    mutating func reset() {
        previousCPU = nil
    }

    mutating func sample() -> SystemReading {
        let now = ContinuousClock.now
        let host = mach_host_self()
        guard host != MACH_PORT_NULL else { return sample(cpu: nil, memory: nil, at: now) }
        defer { _ = mach_port_deallocate(mach_task_self_, host) }
        return sample(cpu: Self.readCPU(host), memory: Self.readMemory(host), at: now)
    }

    mutating func sample(cpu: CPUTicks?, memory: MemoryReading?, at now: ContinuousClock.Instant) -> SystemReading {
        let old = previousCPU
        previousCPU = cpu.map { ($0, now) }
        var reading = SystemReading(memory: memory)
        if let cpu, let old {
            // The monotonic clock includes time asleep. A long gap should not
            // become a "current" CPU reading, even if no wake event arrived.
            let elapsed = old.at.duration(to: now)
            if elapsed > .zero, elapsed <= .seconds(15) {
                reading.cpuPercent = cpu.busyPercent(since: old.ticks)
            }
        }
        return reading
    }

    private static func readCPU(_ host: host_t) -> CPUTicks? {
        var stats = host_cpu_load_info_data_t()
        let wordCount = MemoryLayout<host_cpu_load_info_data_t>.size / MemoryLayout<integer_t>.size
        var count = mach_msg_type_number_t(wordCount)
        let result = withUnsafeMutablePointer(to: &stats) { pointer in
            pointer.withMemoryRebound(to: integer_t.self, capacity: wordCount) {
                host_statistics(host, HOST_CPU_LOAD_INFO, $0, &count)
            }
        }
        guard result == KERN_SUCCESS, count == wordCount else { return nil }
        // The CPU_STATE_* indices are user, system, idle, nice in this order.
        let (user, system, idle, nice) = stats.cpu_ticks
        return CPUTicks(user: user, system: system, idle: idle, nice: nice)
    }

    private static func readMemory(_ host: host_t) -> MemoryReading? {
        guard let wordCount = vmWordCount else { return nil }
        var stats = vm_statistics64_data_t()
        var count = mach_msg_type_number_t(wordCount)
        let result = withUnsafeMutablePointer(to: &stats) { pointer in
            pointer.withMemoryRebound(to: integer_t.self, capacity: wordCount) {
                host_statistics64(host, HOST_VM_INFO64, $0, &count)
            }
        }
        guard result == KERN_SUCCESS, count == wordCount else { return nil }
        // These are host counts, so use the kernel page size, not the task's.
        return MemoryReading.fromPages(
            anonymous: stats.internal_page_count, purgeable: stats.purgeable_count,
            wired: stats.wire_count, compressed: stats.compressor_page_count,
            pageSize: UInt64(vm_kernel_page_size), totalBytes: physicalMemory
        )
    }
}

// Round to the nearest percent, ties up, without overflowing when multiplying.
private func percentage(_ part: UInt64, of whole: UInt64) -> Int {
    let (quotient, remainder) = whole.dividingFullWidth(part.multipliedFullWidth(by: 100))
    return Int(quotient) + (remainder >= whole - remainder ? 1 : 0)
}
