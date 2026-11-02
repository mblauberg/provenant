import Foundation
import ProvenantKit

/// Replays recorded CLI output by argument prefix and records every call. The one replay both test targets share;
/// the recordings are bundle resources. `console-*` files are copies of `tools/front-door/tests/fixtures/console`.
public final class Replay: @unchecked Sendable {
    public typealias Reply = (prefix: [String], file: String, status: Int32)
    private let lock = NSLock()
    private var calls: [[String]] = []
    private var replies: [Reply]

    public init(_ replies: [Reply] = []) { self.replies = replies }

    public static func fixture(_ name: String) throws -> Data {
        guard let url = Bundle.module.url(forResource: name, withExtension: "json", subdirectory: "Fixtures") else {
            throw ClientError.launch("no recorded fixture named \(name)")
        }
        return try Data(contentsOf: url)
    }

    public var recorded: [[String]] { lock.withLock { calls } }
    public func set(_ replies: [Reply]) { lock.withLock { self.replies = replies } }

    public var client: CLIClient {
        CLIClient { [self] arguments, _ in
            let hit = lock.withLock { () -> Reply? in
                calls.append(arguments)
                return replies.first { arguments.starts(with: $0.prefix) }
            }
            guard let hit else { throw ClientError.launch("no recorded reply for \(arguments)") }
            return CLI.Output(status: hit.status, signalled: false, timedOut: false, truncated: false,
                              out: try Self.fixture(hit.file), err: "")
        }
    }
}
