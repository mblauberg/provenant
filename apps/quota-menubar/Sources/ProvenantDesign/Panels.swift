import SwiftUI

/// Shown while a screen's first read is in flight.
public struct LoadingPanel: View {
    let title: String
    public init(_ title: String) { self.title = title }
    public var body: some View {
        ProgressView(title).controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// A read that failed, with the CLI's own repair hint when it gave one and a way to try again.
public struct ErrorPanel: View {
    let title: String, message: String, retry: () -> Void
    public init(_ title: String, message: String, retry: @escaping () -> Void) {
        self.title = title; self.message = message; self.retry = retry
    }
    public var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: "exclamationmark.triangle")
        } description: {
            Text(message)
        } actions: {
            Button("Try Again", action: retry)
                .accessibilityLabel("Try \(title.lowercased()) again")
        }
    }
}

/// Nothing to show, and why.
public struct EmptyPanel: View {
    let title: String, symbol: String, message: String
    public init(_ title: String, symbol: String, message: String) {
        self.title = title; self.symbol = symbol; self.message = message
    }
    public var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: symbol)
        } description: {
            Text(message)
        }
    }
}

/// A one-line notice above content that stays on screen: a failed refresh, partial coverage.
public struct Notice: View {
    let message: String, tone: Tone
    public init(_ message: String, tone: Tone = .caution) { self.message = message; self.tone = tone }
    public var body: some View {
        Label {
            Text(message).font(.callout).fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(tone.color)
        }
        .padding(.horizontal, Space.medium)
        .padding(.vertical, Space.small)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

/// A status as symbol and word together, so colour is never the only signal.
public struct StatusLabel: View {
    let style: StatusStyle
    public init(_ style: StatusStyle) { self.style = style }
    public var body: some View {
        Label(style.label, systemImage: style.symbol)
            .font(.caption.weight(.medium))
            .foregroundStyle(style.tone.color)
    }
}

/// A caption over a value, for detail panes.
public struct Field: View {
    let name: String, value: String
    public init(_ name: String, _ value: String) { self.name = name; self.value = value }
    public var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(name).font(.caption).foregroundStyle(.secondary)
            Text(value).font(.body).textSelection(.enabled)
        }
        .accessibilityElement(children: .combine)
    }
}

/// A count and what it counts, for the overview.
public struct Metric: View {
    let value: String, title: String, symbol: String, tone: Tone
    public init(_ value: String, _ title: String, symbol: String, tone: Tone = .neutral) {
        self.value = value; self.title = title; self.symbol = symbol; self.tone = tone
    }
    public var body: some View {
        VStack(alignment: .leading, spacing: Space.tight) {
            Label(title, systemImage: symbol).font(.callout).foregroundStyle(.secondary)
            Text(value).font(.largeTitle.weight(.semibold)).monospacedDigit().foregroundStyle(tone == .neutral ? Color.primary : tone.color)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(title): \(value)")
    }
}

/// A titled group in a detail pane. The title is a header for VoiceOver's heading navigation.
public struct Block<Content: View>: View {
    let title: String, content: Content
    public init(_ title: String, @ViewBuilder content: () -> Content) { self.title = title; self.content = content() }
    public var body: some View {
        VStack(alignment: .leading, spacing: Space.small) {
            Text(title).font(.headline).accessibilityAddTraits(.isHeader)
            content
        }
    }
}
