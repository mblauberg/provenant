#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;

#[test]
fn published_legacy_initialisation_and_modern_discovery() {
    // 2025-11-25 basic/lifecycle: `Initialization` and `Version Negotiation`:
    // https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle#initialization
    // https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle#version-negotiation
    // 2026-07-28 basic/index: `_meta` and `Per-request protocol fields` define
    // the exact request keys and requirements; server/discover defines the
    // response `_meta` key:
    // https://modelcontextprotocol.io/specification/2026-07-28/basic/index#meta
    // https://modelcontextprotocol.io/specification/2026-07-28/server/discover
    // `Versioning and Compatibility` > `Protocol Version Negotiation` defines
    // UnsupportedProtocolVersionError as -32022:
    // https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning#protocol-version-negotiation
    // `Logging` > `Log Levels` and `Error Handling` define valid logLevel
    // values and recommend rejecting an unknown value with -32602:
    // https://modelcontextprotocol.io/specification/2026-07-28/server/utilities/logging#log-levels
    // https://modelcontextprotocol.io/specification/2026-07-28/server/utilities/logging#error-handling
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
            if modern {
                let metadata = &pair["request"]["params"]["_meta"];
                assert_eq!(
                    metadata,
                    &json!({
                        VERSION:metadata[VERSION],
                        CLIENT_INFO:{"name":"fixture","version":"1"},
                        CAPABILITIES:{},
                        LOG_LEVEL:"info"
                    })
                );
                if metadata[VERSION] == "1900-01-01" {
                    assert_eq!(pair["response"]["error"]["code"], -32022);
                    assert_eq!(
                        pair["response"]["error"]["data"]["supported"],
                        json!([MODERN, LEGACY])
                    );
                }
            }
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
fn invalid_first_request_does_not_commit_an_era() {
    let mut protocol = Protocol::default();
    let Action::Reply(reply) =
        protocol.receive(br#"{"jsonrpc":"2.0","id":"bad-init","method":"initialize","params":{}}"#)
    else {
        panic!()
    };
    assert_eq!(reply["error"]["code"], -32602);

    let valid_modern = json!({
        "jsonrpc":"2.0",
        "id":"discover",
        "method":"server/discover",
        "params":{"_meta":{VERSION:MODERN,CAPABILITIES:{}}}
    });
    let Action::Reply(reply) = protocol.receive(&serde_json::to_vec(&valid_modern).unwrap()) else {
        panic!()
    };
    assert_eq!(
        reply["result"]["supportedVersions"],
        json!([MODERN, LEGACY])
    );
    assert!(protocol.modern());

    let mut protocol = Protocol::default();
    let malformed_modern = json!({
        "jsonrpc":"2.0",
        "id":"bad-discover",
        "method":"server/discover",
        "params":{"_meta":{VERSION:MODERN}}
    });
    let Action::Reply(reply) = protocol.receive(&serde_json::to_vec(&malformed_modern).unwrap())
    else {
        panic!()
    };
    assert_eq!(reply["error"]["code"], -32602);

    let valid_legacy = json!({
        "jsonrpc":"2.0",
        "id":1,
        "method":"initialize",
        "params":{
            "protocolVersion":LEGACY,
            "capabilities":{},
            "clientInfo":{"name":"fixture","version":"1"}
        }
    });
    let Action::Reply(reply) = protocol.receive(&serde_json::to_vec(&valid_legacy).unwrap()) else {
        panic!()
    };
    assert_eq!(reply["result"]["protocolVersion"], LEGACY);
    assert!(!protocol.modern());
}

#[test]
fn modern_tools_list_publishes_only_supported_output_schemas() {
    let mut protocol = Protocol::default();
    let discover = json!({"jsonrpc":"2.0","id":"discover","method":"server/discover",
        "params":{"_meta":{VERSION:MODERN,CAPABILITIES:{}}}});
    assert!(matches!(
        protocol.receive(&serde_json::to_vec(&discover).unwrap()),
        Action::Reply(_)
    ));
    let list = json!({"jsonrpc":"2.0","id":"list","method":"tools/list",
        "params":{"_meta":{VERSION:MODERN,CAPABILITIES:{}}}});
    let Action::Reply(reply) = protocol.receive(&serde_json::to_vec(&list).unwrap()) else {
        panic!("expected tool catalogue")
    };
    let tools = reply["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 11);
    for tool in tools {
        let supported = matches!(
            tool["name"].as_str(),
            Some("fabric_identity" | "fabric_submit")
        );
        assert_eq!(tool.get("outputSchema").is_some(), supported);
        if supported {
            assert_eq!(tool["outputSchema"]["type"], "object");
        }
    }
}

#[test]
fn legacy_tools_list_omits_modern_output_schemas() {
    let mut protocol = Protocol::default();
    let Action::Reply(initialized) = protocol.receive(
        br#"{"jsonrpc":"2.0","id":"initialize","method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}"#,
    ) else {
        panic!("expected legacy initialisation")
    };
    assert_eq!(initialized["result"]["protocolVersion"], LEGACY);
    protocol.receive(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    let Action::Reply(reply) =
        protocol.receive(br#"{"jsonrpc":"2.0","id":"list","method":"tools/list"}"#)
    else {
        panic!("expected legacy tool catalogue")
    };
    let tools = reply["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 11);
    assert!(tools.iter().all(|tool| tool.get("outputSchema").is_none()));
}

#[test]
fn malformed_optional_modern_metadata_does_not_commit_an_era() {
    for metadata in [
        json!({VERSION:MODERN,CAPABILITIES:{},CLIENT_INFO:true}),
        json!({VERSION:MODERN,CAPABILITIES:{},LOG_LEVEL:"verbose"}),
    ] {
        let mut protocol = Protocol::default();
        let request = json!({
            "jsonrpc":"2.0",
            "id":"bad-metadata",
            "method":"server/discover",
            "params":{"_meta":metadata}
        });
        let Action::Reply(reply) = protocol.receive(&serde_json::to_vec(&request).unwrap()) else {
            panic!()
        };
        assert_eq!(reply["error"]["code"], -32602);

        let valid_legacy = json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "protocolVersion":LEGACY,
                "capabilities":{},
                "clientInfo":{"name":"fixture","version":"1"}
            }
        });
        let Action::Reply(reply) = protocol.receive(&serde_json::to_vec(&valid_legacy).unwrap())
        else {
            panic!()
        };
        assert_eq!(reply["result"]["protocolVersion"], LEGACY);
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
    assert_eq!(reply["error"]["code"], -32602);
}

#[test]
fn modern_uncorrelated_errors_omit_id_while_legacy_keeps_null() {
    // MCP 2026-07-28 schema#jsonrpcerrorresponse: id is optional RequestId,
    // which is string | number. JSON-RPC 2.0 section 5 uses null for legacy.
    let mut modern = Protocol::default();
    let request = json!({"jsonrpc":"2.0","id":"discover","method":"server/discover","params":{"_meta":{VERSION:MODERN,CAPABILITIES:{}}}});
    modern.receive(&serde_json::to_vec(&request).unwrap());
    let Action::Reply(reply) = modern.receive(b"{") else {
        panic!()
    };
    assert!(reply.get("id").is_none());
    for id in [json!(null), json!(true), json!(1.5)] {
        let Action::Reply(reply) = modern.receive(
            &serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"method":"tools/list"})).unwrap(),
        ) else {
            panic!()
        };
        assert!(reply.get("id").is_none());
    }
    let Action::Reply(reply) = modern.receive(br#"{"jsonrpc":"wrong","method":"tools/list"}"#)
    else {
        panic!()
    };
    assert!(reply.get("id").is_none());
    let Action::Reply(reply) =
        modern.receive(br#"{"jsonrpc":"wrong","id":"preserved","method":"tools/list"}"#)
    else {
        panic!()
    };
    assert_eq!(reply["id"], "preserved");
    let Action::Reply(reply) = Protocol::default().receive(b"{") else {
        panic!()
    };
    assert_eq!(reply.get("id"), Some(&Value::Null));
}

#[test]
fn invalid_first_tool_calls_do_not_commit_the_protocol_era() {
    for (name, arguments) in [
        ("missing_tool", json!({})),
        ("fabric_identity", json!({"op":"inspect","unknown":true})),
        ("fabric_events", json!({"op":"read","limit":101})),
    ] {
        let mut protocol = Protocol::default();
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "_meta":{VERSION:MODERN,CAPABILITIES:{}},"name":name,"arguments":arguments
        }});
        protocol.receive(&serde_json::to_vec(&request).unwrap());
        assert!(
            !protocol.modern(),
            "invalid first {name} selected the modern era"
        );
        let legacy = json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{
            "protocolVersion":LEGACY,"capabilities":{},"clientInfo":{"name":"fixture","version":"1"}
        }});
        let Action::Reply(reply) = protocol.receive(&serde_json::to_vec(&legacy).unwrap()) else {
            panic!()
        };
        assert_eq!(reply["result"]["protocolVersion"], LEGACY);
    }
}

#[test]
fn modern_results_include_server_info_and_preserve_other_metadata() {
    // MCP 2026-07-28 Basic > `_meta`: servers SHOULD include the exact
    // io.modelcontextprotocol/serverInfo key in every result.
    // https://modelcontextprotocol.io/specification/2026-07-28/basic/index#meta
    let value = json!({"tools":[],"_meta":{"example.test/trace":"retained"}});
    let modern = result(&Id::Signed(1), value.clone(), true);
    assert_eq!(modern["result"]["_meta"][SERVER_INFO], server_info());
    assert_eq!(modern["result"]["_meta"]["example.test/trace"], "retained");
    assert_eq!(
        result(&Id::Signed(1), value.clone(), false)["result"],
        value
    );
}

#[test]
fn configured_json_depth_is_checked_before_protocol_dispatch() {
    let mut protocol = Protocol::default();
    let request=serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":LEGACY,"capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})).unwrap();
    let Action::Reply(reply) = protocol.receive_with_limits(
        &request,
        provenant_records::Limits {
            max_bytes: MAX_FRAME as u64,
            max_depth: 2,
            max_members: 100_000,
        },
    ) else {
        panic!("depth must refuse before dispatch")
    };
    assert_eq!(reply["error"]["code"], -32700);
    assert!(!protocol.modern());
    let Action::Reply(reply) = protocol.receive(&request) else {
        panic!("bounded request remains valid at default depth")
    };
    assert_eq!(reply["result"]["protocolVersion"], LEGACY);
}
