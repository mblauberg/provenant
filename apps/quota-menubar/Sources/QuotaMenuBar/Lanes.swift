import Foundation
import ProvenantKit

enum LaneBoard {
    /// `$PROVENANT_STATE_DIR`, else the default root. A loaded quota report names its own, two levels up.
    static func stateRoot(report: QuotaReport?, environment: [String: String] = ProcessInfo.processInfo.environment) -> URL {
        if let path = report?.reportPath { return URL(fileURLWithPath: path).deletingLastPathComponent().deletingLastPathComponent() }
        return StateDirectory.resolve(environment: environment)
    }
}
