import ProvenantKit

/// Which answers a request offers, by kind. The same set applies whether or not it has options.
enum InboxActions {
    static func isAssumption(_ draft: HumanRequest.Draft) -> Bool {
        draft.kind.lowercased() == "assumption" || draft.proceedingWith != nil
    }

    /// Choose when there are options, confirm and override for an assumption, and reply for every kind.
    static func available(for draft: HumanRequest.Draft) -> [AnswerAction] {
        var actions: [AnswerAction] = []
        if !draft.options.isEmpty { actions.append(.choose) }
        if isAssumption(draft) { actions += [.confirm, .override] }
        actions.append(.reply)
        return actions
    }

    /// The action Return triggers: choose, else confirm, else reply.
    static func primary(for draft: HumanRequest.Draft) -> AnswerAction {
        let actions = available(for: draft)
        return [.choose, .confirm].first(where: actions.contains) ?? .reply
    }
}
