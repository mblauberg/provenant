import SwiftUI
import Combine

// MARK: - Data Models (provenant.quota.report.v1)

struct QuotaReport: Codable {
    let schema: String?
    let generatedAt: String?
    let host: String?
    let enabled: Bool?
    let providers: [String]?
    let accounts: [QuotaAccount]?

    enum CodingKeys: String, CodingKey {
        case schema
        case generatedAt = "generated_at"
        case host, enabled, providers, accounts
    }
}

struct QuotaAccount: Codable, Identifiable {
    var id: String { "\(provider):\(account)" }
    let provider: String
    let account: String
    let plan: String?
    let active: Bool?
    let status: String?
    let error: String?
    let state: String?
    let summary: String?
    let windows: [QuotaWindow]?

    enum CodingKeys: String, CodingKey {
        case provider, account, plan, active, status, error, state, summary, windows
    }
}

struct QuotaWindow: Codable, Identifiable {
    var id: String { name }
    let name: String
    let usedPercent: Double?
    let windowMinutes: Int?
    let resetAt: String?
    let pace: QuotaPace?

    enum CodingKeys: String, CodingKey {
        case name
        case usedPercent = "used_percent"
        case windowMinutes = "window_minutes"
        case resetAt = "reset_at"
        case pace
    }
}

struct QuotaPace: Codable {
    let state: String?
    let summary: String?
    let deltaPercent: Double?
    let headroomPercent: Double?
    let willLastToReset: Bool?

    enum CodingKeys: String, CodingKey {
        case state, summary
        case deltaPercent = "delta_percent"
        case headroomPercent = "headroom_percent"
        case willLastToReset = "will_last_to_reset"
    }
}

// MARK: - Formatters

func formatReset(_ isoString: String?) -> String {
    guard let isoString = isoString, !isoString.isEmpty else { return "-" }
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    var date = formatter.date(from: isoString)
    if date == nil {
        formatter.formatOptions = [.withInternetDateTime]
        date = formatter.date(from: isoString)
    }
    guard let target = date else {
        return String(isoString.prefix(16).replacingOccurrences(of: "T", with: " "))
    }
    let diff = target.timeIntervalSince(Date())
    if diff <= 0 { return "now" }
    let hours = Int(diff) / 3600
    let minutes = (Int(diff) % 3600) / 60
    if hours >= 48 {
        let days = hours / 24
        let remH = hours % 24
        return "\(days)d \(remH)h"
    } else if hours > 0 {
        return "\(hours)h \(minutes)m"
    } else {
        return "\(minutes)m"
    }
}

func deficitMarker(state: String?) -> String {
    switch state?.lowercased() {
    case "deficit":
        return " [deficit]"
    case "exhausted":
        return " [exhausted]"
    default:
        return ""
    }
}

// MARK: - Quota Model & CLI Fetcher

final class QuotaModel: ObservableObject {
    @Published var title: String = "Q --%"
    @Published var accounts: [QuotaAccount] = []
    @Published var statusText: String = "Loading quota..."
    @Published var lastUpdated: String? = nil

    private var cancellables = Set<AnyCancellable>()
    private let intervalSeconds: TimeInterval

    init() {
        if let raw = ProcessInfo.processInfo.environment["QUOTA_INTERVAL_SECONDS"],
           let val = Double(raw), val >= 5 {
            self.intervalSeconds = val
        } else {
            self.intervalSeconds = 120
        }

        refresh()

        Timer.publish(every: intervalSeconds, on: .main, in: .common)
            .autoconnect()
            .sink { [weak self] _ in self?.refresh() }
            .store(in: &cancellables)
    }

    func refresh() {
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let report = Self.fetchReport()
            DispatchQueue.main.async {
                self?.applyReport(report)
            }
        }
    }

    private func applyReport(_ report: QuotaReport?) {
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm:ss"
        lastUpdated = formatter.string(from: Date())

        guard let report = report else {
            title = "Q !"
            statusText = "Failed to run `provenant quota --json`"
            return
        }

        if report.enabled == false {
            title = "Q off"
            statusText = "Quota fetching disabled"
            accounts = report.accounts ?? []
            return
        }

        let accts = report.accounts ?? []
        self.accounts = accts

        let allWindows = accts.flatMap { $0.windows ?? [] }
        let maxUsed = allWindows.compactMap { $0.usedPercent }.max()

        if let maxUsed = maxUsed {
            title = "Q \(Int(round(maxUsed)))%"
        } else if accts.isEmpty {
            title = "Q --%"
            statusText = "No accounts configured"
        } else {
            title = "Q 0%"
        }
    }

    /// Shells out to `provenant quota --json`.
    /// Never touches credential files or tokens directly.
    private static func fetchReport() -> QuotaReport? {
        let process = Process()
        let pipe = Pipe()

        let bin = ProcessInfo.processInfo.environment["PROVENANT_BIN"]
        if let customBin = bin, FileManager.default.isExecutableFile(atPath: customBin) {
            process.executableURL = URL(fileURLWithPath: customBin)
            process.arguments = ["quota", "--json"]
        } else {
            process.executableURL = URL(fileURLWithPath: "/bin/zsh")
            process.arguments = ["-lc", "provenant quota --json"]
        }

        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice

        do {
            try process.run()
            // Drain before waiting, or output past the pipe buffer deadlocks the child.
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            guard process.terminationStatus == 0 else { return nil }
            return try JSONDecoder().decode(QuotaReport.self, from: data)
        } catch {
            return nil
        }
    }
}

// MARK: - SwiftUI Menu & App

struct QuotaMenuView: View {
    @ObservedObject var model: QuotaModel

    var body: some View {
        if model.accounts.isEmpty {
            Text(model.statusText)
        } else {
            ForEach(model.accounts) { account in
                let planStr = account.plan.map { " (\($0))" } ?? ""
                let activeStr = (account.active == true) ? "*" : ""
                Text("\(account.provider)/\(account.account)\(activeStr)\(planStr)")
                    .font(.headline)
                if let windows = account.windows, !windows.isEmpty {
                    ForEach(windows) { win in
                        let used = Int(round(win.usedPercent ?? 0))
                        let reset = formatReset(win.resetAt)
                        let marker = deficitMarker(state: win.pace?.state ?? account.state)
                        Text("  \(win.name): \(used)% • resets \(reset)\(marker)")
                    }
                } else {
                    let err = account.error ?? account.status ?? "unavailable"
                    Text("  status: \(err)")
                }
                Divider()
            }
        }

        if let updated = model.lastUpdated {
            Text("Updated: \(updated)")
            Divider()
        }

        Button("Refresh") {
            model.refresh()
        }
        .keyboardShortcut("r")

        Divider()

        Button("Quit") {
            NSApplication.shared.terminate(nil)
        }
        .keyboardShortcut("q")
    }
}

@main
struct QuotaMenuBarApp: App {
    @StateObject private var model = QuotaModel()

    init() {
        NSApplication.shared.setActivationPolicy(.accessory)
    }

    var body: some Scene {
        MenuBarExtra(model.title) {
            QuotaMenuView(model: model)
        }
        .menuBarExtraStyle(.menu)
    }
}
