import Foundation

/// A CLI error as the envelope carries it.
public struct CommandError: Decodable, Equatable, Sendable {
    public let code: String
    public let message: String
    public let repair: String?
}

/// The `{schema_version, command, status, data, error}` envelope every `--json` command prints.
public struct Envelope<Payload: Decodable & Sendable>: Decodable, Sendable {
    public let schemaVersion: UInt64
    public let command: String
    public let status: String
    public let data: Payload?
    public let error: CommandError?

    public static func decode(_ bytes: Data) throws -> Envelope {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Envelope.self, from: bytes)
    }
}

/// Why a CLI call gave no usable data.
public enum ClientError: Error, Equatable, Sendable {
    case notFound
    case launch(String)
    case timedOut
    case signalled(Int32)
    case exited(status: Int32, detail: String)
    case outputTooLarge
    case malformed(String)
    /// The CLI answered with an error envelope; `code` is its stable error code.
    case command(CommandError)

    public var code: String? { if case .command(let error) = self { error.code } else { nil } }

    public var message: String {
        switch self {
        case .notFound:
            "provenant was not found on the login shell's PATH or at ~/.local/bin/provenant. Set PROVENANT_BIN to its path."
        case .launch(let why): "Could not start provenant: \(why)"
        case .timedOut: "provenant timed out."
        case .signalled(let signal): "provenant was killed by signal \(signal)."
        case .exited(let status, let detail): "provenant exited with status \(status): \(detail)"
        case .outputTooLarge: "provenant printed more output than the app reads."
        case .malformed(let why): "provenant returned an unreadable response: \(why)"
        case .command(let error): [error.message, error.repair].compactMap { $0 }.joined(separator: " ")
        }
    }
}

/// Calls the CLI off the main thread and decodes its envelope. One client keeps the resolved launcher.
public final class CLIClient: @unchecked Sendable {
    public typealias Runner = @Sendable (_ arguments: [String], _ limit: Double) throws -> CLI.Output

    private let lock = NSLock()
    private var launcher: CLI.Launcher?
    private let runner: Runner?
    private let limit: Double

    public init(limit: Double = 30) { runner = nil; self.limit = limit }

    /// A client over a fixed runner, such as one that replays recorded CLI output in tests.
    public init(limit: Double = 30, runner: @escaping Runner) { self.runner = runner; self.limit = limit }

    /// Runs `arguments` plus `--json` and decodes the envelope. Error envelopes throw `ClientError.command`, whether
    /// or not the CLI exited cleanly.
    public func call<Payload: Decodable & Sendable>(_ arguments: [String], as _: Payload.Type = Payload.self) async throws -> Payload {
        try await Task.detached(priority: .userInitiated) { [self] in try callSync(arguments, as: Payload.self) }.value
    }

    func callSync<Payload: Decodable & Sendable>(_ arguments: [String], as _: Payload.Type) throws -> Payload {
        let run = try execute(arguments + ["--json"])
        return try Self.decode(run, command: nil, as: Payload.self)
    }

    /// Decode an already scheduled CLI call, including structured errors on nonzero exits.
    public static func decode<Payload: Decodable & Sendable>(_ run: CLI.Output, command: String?, as _: Payload.Type) throws -> Payload {
        if run.truncated { throw ClientError.outputTooLarge }
        if run.timedOut { throw ClientError.timedOut }
        if run.signalled { throw ClientError.signalled(run.status) }
        let envelope: Envelope<Payload>
        do { envelope = try Envelope<Payload>.decode(run.out) } catch {
            if run.status != 0 {
                throw ClientError.exited(status: run.status, detail: run.err.split(separator: "\n").last.map(String.init) ?? "no error output")
            }
            throw ClientError.malformed(Self.describe(error))
        }
        if let command, (envelope.schemaVersion != 1 || envelope.command != command) {
            throw ClientError.malformed("unexpected command envelope")
        }
        if let error = envelope.error { throw ClientError.command(error) }
        if run.status != 0 {
            throw ClientError.exited(status: run.status, detail: run.err.split(separator: "\n").last.map(String.init) ?? "no error output")
        }
        guard envelope.status == "ok", let data = envelope.data else {
            throw ClientError.malformed("status \(envelope.status) without data")
        }
        return data
    }

    private func execute(_ arguments: [String]) throws -> CLI.Output {
        if let runner { return try runner(arguments, limit) }
        let known = lock.withLock { launcher }
        guard let found = known ?? CLI.resolve() else { throw ClientError.notFound }
        lock.withLock { launcher = found }
        do { return try CLI.exec(found.url, arguments, path: found.path, limit: limit) } catch {
            lock.withLock { launcher = nil }
            throw ClientError.launch(error.localizedDescription)
        }
    }

    static func describe(_ error: Error) -> String {
        guard let error = error as? DecodingError else { return error.localizedDescription }
        switch error {
        case .keyNotFound(let key, _): return "\(key.stringValue) is missing"
        case .typeMismatch(_, let context), .valueNotFound(_, let context), .dataCorrupted(let context):
            let path = context.codingPath.map(\.stringValue).joined(separator: ".")
            return path.isEmpty ? context.debugDescription : "\(path): \(context.debugDescription)"
        @unknown default: return error.localizedDescription
        }
    }
}

/// A page of a cursor-paged list.
public struct Page<Item: Sendable>: Sendable {
    public var items: [Item]
    public var nextCursor: String?
    public var truncated: Bool
}

/// Reads pages until the cursor runs out or `maxPages` is reached. `truncated` is set when pages remain.
public func collectPages<Item: Sendable>(maxPages: Int = 10, _ fetch: (String?) async throws -> Page<Item>) async throws -> Page<Item> {
    var all: [Item] = [], cursor: String?, more = false
    for _ in 0..<maxPages {
        let page = try await fetch(cursor)
        all += page.items
        cursor = page.nextCursor
        more = page.truncated || cursor != nil
        if cursor == nil { break }
    }
    return Page(items: all, nextCursor: cursor, truncated: more)
}
