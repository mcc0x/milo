@main struct MenuResourcesTest {
    static func main() {
        cpuSamples()
        cpuDiscontinuities()
        memoryPages()
    }

    private static func cpuSamples() {
        var sampler = SystemSampler()
        let start = ContinuousClock.now
        let memory = MemoryReading(usedBytes: 32_768, totalBytes: 131_072, percent: 25)
        func sample(_ ticks: SystemSampler.CPUTicks?, _ seconds: Int, memory: MemoryReading?) -> SystemReading {
            sampler.sample(cpu: ticks, memory: memory, at: start.advanced(by: .seconds(seconds)))
        }

        // Memory is already available while CPU primes its first baseline.
        precondition(sample(ticks(100, 50, 400, 20), 0, memory: memory) == SystemReading(memory: memory))
        // All three non-idle states contribute; 5/8 rounds up to 63%.
        precondition(sample(ticks(102, 52, 403, 21), 5, memory: memory) == SystemReading(cpuPercent: 63, memory: memory))
        precondition(sample(ticks(102, 52, 403, 21), 10, memory: memory) == SystemReading(memory: memory))
        precondition(sample(ticks(102, 52, 410, 21), 15, memory: memory).cpuPercent == 0)
        precondition(sample(ticks(103, 52, 410, 21), 20, memory: memory).cpuPercent == 100)

        precondition(sample(nil, 25, memory: memory) == SystemReading(memory: memory))
        precondition(sample(ticks(104, 52, 410, 21), 30, memory: memory).cpuPercent == nil)
        precondition(sample(ticks(105, 52, 411, 21), 35, memory: nil) == SystemReading(cpuPercent: 50))
        precondition(sample(ticks(106, 52, 412, 21), 40, memory: memory).cpuPercent == 50)
        sampler.reset()
        precondition(sample(ticks(107, 52, 413, 21), 45, memory: memory).cpuPercent == nil)
        precondition(sample(ticks(108, 52, 414, 21), 50, memory: memory).cpuPercent == 50)
    }

    private static func cpuDiscontinuities() {
        var sampler = SystemSampler()
        let start = ContinuousClock.now
        func cpu(_ ticks: SystemSampler.CPUTicks, _ seconds: Int) -> Int? {
            sampler.sample(cpu: ticks, memory: nil, at: start.advanced(by: .seconds(seconds))).cpuPercent
        }

        let max = UInt32.max
        precondition(cpu(ticks(0, 0, 0, 0), 0) == nil)
        // Summing four UInt32 counters can overflow before calculating a ratio.
        precondition(cpu(ticks(max, max, max, max), 15) == 75)
        precondition(cpu(ticks(max, max, 0, max), 20) == nil) // idle counter wrapped
        precondition(cpu(ticks(max, max, 1, max), 25) == 0)
        precondition(cpu(ticks(35, 40, 42, 0), 30) == nil) // counters reset
        precondition(cpu(ticks(37, 40, 44, 0), 35) == 50)
        precondition(cpu(ticks(39, 40, 46, 0), 51) == nil) // elapsed > 15 seconds
        precondition(cpu(ticks(41, 40, 48, 0), 56) == 50)
        precondition(cpu(ticks(42, 40, 49, 0), 56) == nil) // clock did not advance
    }

    private static func memoryPages() {
        let page: UInt64 = 16_384
        func memory(_ anonymous: UInt32, _ purgeable: UInt32, _ wired: UInt32, _ compressed: UInt32) -> MemoryReading? {
            .fromPages(anonymous: anonymous, purgeable: purgeable, wired: wired, compressed: compressed,
                       pageSize: page, totalBytes: 8 * page)
        }
        precondition(memory(4, 2, 2, 1) == MemoryReading(usedBytes: 5 * page, totalBytes: 8 * page, percent: 63))
        precondition(memory(4, 4, 0, 0) == MemoryReading(usedBytes: 0, totalBytes: 8 * page, percent: 0))
        precondition(memory(2, 0, 4, 2) == MemoryReading(usedBytes: 8 * page, totalBytes: 8 * page, percent: 100))
        precondition(memory(2, 3, 1, 1) == nil) // invalid subtraction
        precondition(memory(4, 0, 4, 1) == nil) // more than physical memory

        func bytes(_ count: UInt32, _ pageSize: UInt64, _ total: UInt64) -> MemoryReading? {
            .fromPages(anonymous: count, purgeable: 0, wired: 0, compressed: 0, pageSize: pageSize, totalBytes: total)
        }
        precondition(bytes(1, 0, page) == nil)
        precondition(bytes(1, page, 0) == nil)
        precondition(bytes(2, UInt64.max, UInt64.max) == nil) // byte conversion overflow
        precondition(bytes(1, UInt64.max, UInt64.max)?.percent == 100) // percent scaling past UInt64
        let total: UInt64 = 10_000_000_000_000_000_000
        let tie: UInt64 = 5_050_000_000_000_000_000
        precondition(bytes(1, tie - 1, total)?.percent == 50)
        precondition(bytes(1, tie, total)?.percent == 51)
    }

    private static func ticks(_ user: UInt32, _ system: UInt32, _ idle: UInt32, _ nice: UInt32) -> SystemSampler.CPUTicks {
        .init(user: user, system: system, idle: idle, nice: nice)
    }
}
