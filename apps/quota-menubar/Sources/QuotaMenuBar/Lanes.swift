import Foundation

/// A lane (one attempt) as `provenant lanes --all --json` reports it, reduced to what the panel needs.
struct Lane: Sendable, Equatable {
    let runID: String, attemptID: String, state: String, route: String
    let queuedAt: Date?, startedAt: Date?, finishedAt: Date?
}

/// Why a lane is in the panel. The order is the panel's order.
enum Attention: Int, Comparable {
    case inputRequired, failed, stalled
    static func < (a: Attention, b: Attention) -> Bool { a.rawValue < b.rawValue }

    var title: String {
        switch self {
        case .inputRequired: "Input required"
        case .failed: "Failed"
        case .stalled: "Stalled"
        }
    }
    var symbol: String {
        switch self {
        case .inputRequired: "questionmark.bubble.fill"
        case .failed: "exclamationmark.octagon.fill"
        case .stalled: "hourglass"
        }
    }
}

/// A lane that needs a look, with the instant its state began.
struct AttentionLane: Identifiable, Equatable {
    let lane: Lane, kind: Attention, since: Date
    var id: String { lane.attemptID }
}

enum LaneBoard {
    /// Terminal outcomes that count as a failure; `succeeded` and `cancelled` never need attention.
    static let failures: Set<String> = [
        "failed", "rejected", "timed_out", "rate_limited", "quota_limited", "auth_required",
        "model_unavailable", "permission_denied", "quarantined", "lost",
    ]
    /// Nonterminal states a lane passes through quickly. The run store keeps no output-activity time, so a lane
    /// held in one of them past `stallAfter` is the only silence the CLI lets the app see.
    static let transitional: Set<String> = ["queued", "starting", "settling"]
    static let stallAfter: TimeInterval = 600, failureWindow: TimeInterval = 3600, inputWindow: TimeInterval = 86_400

    /// Lanes that need attention, input first, then failures, then stalls; the newest first within each.
    static func attention(_ lanes: [Lane], now: Date) -> [AttentionLane] {
        lanes.compactMap { lane -> AttentionLane? in
            if lane.state == "input_required", let at = lane.finishedAt, now.timeIntervalSince(at) < inputWindow {
                return AttentionLane(lane: lane, kind: .inputRequired, since: at)
            }
            if failures.contains(lane.state), let at = lane.finishedAt, now.timeIntervalSince(at) < failureWindow {
                return AttentionLane(lane: lane, kind: .failed, since: at)
            }
            if transitional.contains(lane.state), let at = lane.startedAt ?? lane.queuedAt, now.timeIntervalSince(at) >= stallAfter {
                return AttentionLane(lane: lane, kind: .stalled, since: at)
            }
            return nil
        }.sorted { ($0.kind, $1.since) < ($1.kind, $0.since) }
    }

    /// The run directory the CLI keeps under the state root: `<state root>/runs/<run id>`.
    static func runDirectory(_ lane: Lane, stateRoot: URL) -> URL {
        stateRoot.appending(path: "runs").appending(path: lane.runID)
    }

    /// `$PROVENANT_STATE_DIR`, else the default root. A loaded quota report names its own, two levels up.
    static func stateRoot(report: QuotaReport?, environment: [String: String] = ProcessInfo.processInfo.environment) -> URL {
        if let path = report?.reportPath { return URL(fileURLWithPath: path).deletingLastPathComponent().deletingLastPathComponent() }
        if let dir = environment["PROVENANT_STATE_DIR"], dir.hasPrefix("/") { return URL(fileURLWithPath: dir) }
        return FileManager.default.homeDirectoryForCurrentUser.appending(path: ".local/share/provenant/v1")
    }
}

private struct LaneWire: Decodable {
    struct Route: Decodable { let model: String? }
    let runId: String, attemptId: String, state: String
    let appliedRoute: Route?
    let queuedAt: String?, startedAt: String?, finishedAt: String?
}

private struct LanePageWire: Decodable {
    let lanes: [LaneWire]
    let nextCursor: String?
}

private struct LaneEnvelope: Decodable {
    struct Failure: Decodable { let message: String }
    let schemaVersion: UInt64
    let command, status: String
    let data: LanePageWire?
    let error: Failure?
}

extension Lane {
    /// Decodes one `lanes` response and returns its lanes and the cursor of the next page.
    static func decodePage(_ data: Data) throws -> (lanes: [Lane], next: String?) {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let envelope = try decoder.decode(LaneEnvelope.self, from: data)
        guard envelope.schemaVersion == 1, envelope.command == "lanes", envelope.status == "ok", let page = envelope.data else {
            throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: envelope.error?.message ?? "unexpected lanes envelope"))
        }
        let whole = ISO8601DateFormatter(), fraction = ISO8601DateFormatter()
        fraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        func date(_ text: String?) -> Date? { text.flatMap { fraction.date(from: $0) ?? whole.date(from: $0) } }
        let lanes = page.lanes.map {
            Lane(runID: $0.runId, attemptID: $0.attemptId, state: $0.state, route: $0.appliedRoute?.model ?? "unresolved",
                 queuedAt: date($0.queuedAt), startedAt: date($0.startedAt), finishedAt: date($0.finishedAt))
        }
        return (lanes, page.nextCursor)
    }
}

extension CLI {
    /// Reads every lane through `provenant lanes --all`, page by page, off the main thread. Only lanes that could
    /// need attention are kept. A failed page fails the read, so a partial list never replaces a good one.
    static func lanes(_ cached: Launcher?) -> (Launcher?, Result<[Lane], Failure>) {
        guard let launcher = cached ?? resolve() else { return (nil, .failure(Failure(message: "provenant was not found"))) }
        var kept: [Lane] = [], cursor: String?
        for _ in 0..<20 {
            let arguments = ["lanes", "--all", "--limit", "100", "--json"] + (cursor.map { ["--cursor", $0] } ?? [])
            do {
                let run = try exec(launcher.url, arguments, path: launcher.path, limit: 20)
                guard run.status == 0, !run.timedOut, !run.signalled else {
                    return (launcher, .failure(Failure(message: "provenant lanes exited with status \(run.status)")))
                }
                let page = try Lane.decodePage(run.out)
                kept += page.lanes.filter { !["succeeded", "cancelled"].contains($0.state) }
                guard let next = page.next else { return (launcher, .success(kept)) }
                cursor = next
            } catch {
                return (launcher, .failure(Failure(message: "provenant lanes: \(error.localizedDescription)")))
            }
        }
        return (launcher, .success(kept))
    }
}
