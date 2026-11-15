use super::*;

/// `stop` inside a lane: the graph owner stops the named node's subtree. A
/// lane may stop only its own children, which the relay checks.
pub fn stop_from_lane(target: &Target) -> Reply {
    let node = match &target.node {
        Some(node) => format!("{}/{node}", target.name),
        None => target.name.clone(),
    };
    let cancel = CancelToken::new();
    let result = crate::fabric::graph_relay::from_lane(
        &provenant_fabric_runtime::graph::relay::RelayOp::Stop { node },
        &cancel,
        Duration::from_secs(55),
    )
    .map(|value| (value, "Subtree stop requested\n".into()))
    .map_err(|error| {
        Fail::new(
            Status::Refused,
            ErrorKind::LaneCoordinationRefused,
            error.to_string(),
        )
    });
    reply("stop", result)
}
