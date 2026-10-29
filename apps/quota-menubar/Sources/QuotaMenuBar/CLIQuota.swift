import Foundation
import ProvenantKit

/// The quota and settings commands the menu runs.
extension CLI {
    enum Outcome { case report(QuotaReport), failure(Failure) }

    static func quota(_ cached: Launcher?, _ arguments: [String]) -> (Launcher?, Outcome) {
        let operation = arguments.dropFirst().first ?? "tick"
        func fail(_ launcher: Launcher?, _ cause: String, _ message: String) -> (Launcher?, Outcome) {
            log.error("quota \(operation, privacy: .public) failed: \(cause, privacy: .public)")
            return (launcher, .failure(Failure(message: message)))
        }
        guard let launcher = cached ?? resolve() else {
            return fail(nil, "not found", "provenant was not found on the login shell's PATH or at ~/.local/bin/provenant. "
                        + "Set PROVENANT_BIN to its path.")
        }
        let name = launcher.custom ? "PROVENANT_BIN (\(launcher.url.path))" : "provenant"
        let run: Output
        do { run = try exec(launcher.url, arguments, path: launcher.path, limit: 60) } catch {
            return fail(nil, "launch", "Could not start \(name): \(error.localizedDescription)")
        }
        let detail = run.err.split(separator: "\n").last.map(String.init) ?? "no error output"
        if run.timedOut { return fail(launcher, "timeout", "provenant quota timed out after 60 seconds.") }
        if run.signalled { return fail(launcher, "signal \(run.status)", "provenant quota was killed by signal \(run.status).") }
        if run.status == 127 {
            let hint = launcher.custom ? "" : " Check that the login shell's PATH reaches it, or set PROVENANT_BIN."
            return fail(nil, "status 127", "\(name) exited with status 127: \(detail).\(hint)")
        }
        if run.status != 0 { return fail(launcher, "status \(run.status)", "\(name) exited with status \(run.status): \(detail)") }
        switch decodeEnvelope(run.out, command: "quota \(operation)") {
        case .success(let report): return (launcher, .report(report))
        case .failure(let failure): return fail(launcher, "unreadable response", failure.message)
        }
    }

    /// Runs a command whose output the app does not read; nil on success.
    static func command(_ cached: Launcher?, _ arguments: [String]) -> (Launcher?, Failure?) {
        guard let launcher = cached ?? resolve() else {
            return (nil, Failure(message: "provenant was not found on the login shell's PATH or at ~/.local/bin/provenant. "
                                 + "Set PROVENANT_BIN to its path."))
        }
        let name = arguments.prefix(2).joined(separator: " ")
        let run: Output
        do { run = try exec(launcher.url, arguments, path: launcher.path, limit: 30) } catch {
            return (nil, Failure(message: "Could not start provenant: \(error.localizedDescription)"))
        }
        if run.status == 0 && !run.timedOut && !run.signalled { return (launcher, nil) }
        let detail = run.err.split(separator: "\n").last.map(String.init) ?? "no error output"
        log.error("\(name, privacy: .public) failed with status \(run.status)")
        return (launcher, Failure(message: "provenant \(name) failed: \(detail)"))
    }


    static func decode(_ data: Data) -> Result<QuotaReport, Failure> {
        do { return .success(try QuotaReport.decode(data)) } catch {
            let (keys, why): ([CodingKey], String) = switch error as? DecodingError {
            case .keyNotFound(let key, let context): (context.codingPath + [key], "is missing")
            case .typeMismatch(_, let context), .valueNotFound(_, let context), .dataCorrupted(let context):
                (context.codingPath, "is unreadable: \(context.debugDescription)")
            default: ([], "is unreadable: \(error.localizedDescription)")
            }
            let path = keys.map { $0.intValue.map { "[\($0)]" } ?? ".\($0.stringValue)" }.joined().trimmingCharacters(in: ["."])
            return .failure(Failure(message: "The report\(path.isEmpty ? "" : " field \(path)") \(why)"))
        }
    }

    static func decodeEnvelope(_ data: Data, command: String) -> Result<QuotaReport, Failure> {
        do { return .success(try QuotaReport.decodeEnvelope(data, command: command)) } catch {
            return .failure(Failure(message: "The quota command returned an unreadable response: \(error.localizedDescription)"))
        }
    }

}
