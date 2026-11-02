import Foundation
import Observation
import ProvenantKit

/// One screen's read of the CLI: the last good value, whether a read is running and the last failure. A failed read
/// leaves the previous value on screen. A read asked for while one runs is folded into one more run, so a change
/// that lands mid-read is never lost and reads never stack.
@MainActor @Observable
final class Reader<Value: Equatable & Sendable> {
    private(set) var value: Value?
    private(set) var loading = false
    private(set) var error: String?
    private(set) var loadedAt: Date?
    @ObservationIgnored private var again = false

    init(value: Value? = nil) { self.value = value }

    /// True until the first read has finished, either way.
    var isFirstLoad: Bool { value == nil && error == nil }

    func load(now: @MainActor () -> Date = { .now }, _ operation: @MainActor () async throws -> Value) async {
        guard !loading else { again = true; return }
        loading = true
        defer { loading = false }
        repeat {
            again = false
            do {
                value = try await operation()
                error = nil
                loadedAt = now()
            } catch {
                self.error = Self.describe(error)
            }
        } while again
    }

    /// Swaps in a value built from the current one, such as a longer page list.
    func replace(_ value: Value) { self.value = value }

    /// Forgets the value, for a read whose subject changed.
    func reset() { value = nil; error = nil; loadedAt = nil }

    static func describe(_ error: Error) -> String { (error as? ClientError)?.message ?? error.localizedDescription }
}
