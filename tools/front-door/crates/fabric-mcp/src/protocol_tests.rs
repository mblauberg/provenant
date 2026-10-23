#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;

#[test]
fn published_legacy_initialisation_and_modern_discovery() {
    // https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle#initialization
    // https://modelcontextprotocol.io/specification/2026-07-28/server/discover
    // MCP 2025-11-25 basic/lifecycle: `Initialization`; 2026-07-28
    // server/discover: DiscoverResult and basic/versioning: Backward Compatibility.
    for (fixture, modern) in [
        (
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/legacy.jsonl"
            )),
            false,
        ),
        (
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/modern.jsonl"
            )),
            true,
        ),
    ] {
        let mut protocol = Protocol::default();
        for line in fixture.lines() {
            let pair: Value = serde_json::from_str(line).unwrap();
            let action = protocol.receive(serde_json::to_vec(&pair["request"]).unwrap().as_slice());
            let Action::Reply(reply) = action else {
                panic!("expected reply")
            };
            assert_eq!(reply, pair["response"]);
        }
        assert_eq!(protocol.modern(), modern);
    }
}
#[test]
fn lifecycle_versions_ids_and_strict_envelopes() {
    let mut protocol = Protocol::default();
    assert!(matches!(
        protocol.receive(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#),
        Action::Reply(_)
    ));
    let Action::Reply(bad) = protocol.receive(b"{") else {
        panic!()
    };
    assert_eq!(bad["error"]["code"], -32700);
    for id in [json!(null), json!(true), json!(1.5)] {
        let Action::Reply(bad) = protocol.receive(
            &serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"method":"ping"})).unwrap(),
        ) else {
            panic!()
        };
        assert_eq!(bad["error"]["code"], -32600);
    }
    let mut protocol = Protocol::default();
    let request = json!({"jsonrpc":"2.0","id":18446744073709551615u64,"method":"server/discover","params":{"_meta":{VERSION:"unreleased",CAPABILITIES:{}}}});
    let Action::Reply(reply) = protocol.receive(&serde_json::to_vec(&request).unwrap()) else {
        panic!()
    };
    assert_eq!(reply["id"], request["id"]);
    assert_eq!(reply["error"]["code"], -32022);
    assert_eq!(reply["error"]["data"]["supported"], json!([MODERN, LEGACY]));
    let Action::Reply(reply) =
        protocol.receive(br#"{"jsonrpc":"2.0","id":"switch","method":"initialize","params":{}}"#)
    else {
        panic!()
    };
    assert_eq!(reply["error"]["code"], -32600);
}
