import Foundation

/// The places the sidebar goes. ⌘1 to ⌘5 select the first five; search results appear only after a search.
enum Destination: String, CaseIterable, Identifiable, Sendable {
    case overview, inbox, runs, quota, chairs, search

    var id: String { rawValue }

    /// The destinations with a fixed sidebar row and a number shortcut, in order.
    static let numbered: [Destination] = [.overview, .inbox, .runs, .quota, .chairs]

    var title: String {
        switch self {
        case .overview: "Overview"
        case .inbox: "Inbox"
        case .runs: "Runs"
        case .quota: "Quota"
        case .chairs: "Chairs & Projects"
        case .search: "Search Results"
        }
    }

    var symbol: String {
        switch self {
        case .overview: "rectangle.grid.2x2"
        case .inbox: "tray"
        case .runs: "bolt.horizontal"
        case .quota: "gauge.with.dots.needle.50percent"
        case .chairs: "person.2"
        case .search: "magnifyingglass"
        }
    }

    /// The digit of the ⌘ shortcut, when it has one.
    var shortcut: Character? { Destination.numbered.firstIndex(of: self).map { Character(String($0 + 1)) } }
}

/// A sidebar row. The Inbox has a second level of filters and projects; every other destination is one row.
enum SidebarItem: Hashable, Sendable {
    case overview, runs, quota, chairs, search
    case inbox(InboxScope)

    var destination: Destination {
        switch self {
        case .overview: .overview
        case .inbox: .inbox
        case .runs: .runs
        case .quota: .quota
        case .chairs: .chairs
        case .search: .search
        }
    }

    init(_ destination: Destination) {
        switch destination {
        case .overview: self = .overview
        case .inbox: self = .inbox(.all)
        case .runs: self = .runs
        case .quota: self = .quota
        case .chairs: self = .chairs
        case .search: self = .search
        }
    }
}

/// What the window's search field searches.
enum SearchScope: String, CaseIterable, Identifiable, Sendable {
    case requests, prompts, results
    var id: String { rawValue }
    var title: String {
        switch self {
        case .requests: "Requests"
        case .prompts: "Prompts"
        case .results: "Results"
        }
    }
    var part: SearchModel.Part? {
        switch self {
        case .requests: nil
        case .prompts: .prompt
        case .results: .result
        }
    }
}
