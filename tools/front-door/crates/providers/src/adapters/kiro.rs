use super::*;

pub fn kiro(plan: &Plan) -> Result<AdapterProfile, BuildError> {
    let mut argv = vec![
        "chat".into(),
        "--no-interactive".into(),
        "--agent-engine".into(),
        "v3".into(),
        "--output-format".into(),
        "stream-json".into(),
    ];
    if plan.mode.is_writer() {
        argv.push("--trust-all-tools".into());
    } else {
        argv.push("--trust-tools=fs_read,grep,glob".into());
    }
    if let Some(session) = plan
        .resume_session
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        argv.extend(["--resume-id".into(), session.clone()]);
    }
    if let Some(model) = plan
        .model
        .as_ref()
        .filter(|model| !model.is_empty() && model.as_str() != "auto")
    {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = plan.effort.as_ref().filter(|value| !value.is_empty()) {
        argv.extend(["--effort".into(), effort.clone()]);
    }
    argv.push(format!("{}\n\n{}", plan.boundary_prompt, plan.prompt));
    profile("kiro", argv)
}

/// Whether the router's version-bound negative write probe is still fresh.
/// `now` is explicit so policy tests never depend on wall-clock timing.
/// Construct routes with exact i64/u64 integers; decode dynamic JSON through
/// [`crate::decode_json`] (or [`read_only_guarantee_json`]) before calling this
/// value API. An already-rounded serde_json Value cannot recover its input.
pub fn read_only_guarantee(route: &serde_json::Value, now: jiff::Timestamp) -> &'static str {
    let valid = (|| {
        let probe = route.get("read_only_probe")?;
        let checked_at = provenant_core::py::python_iso_timestamp(
            &probe.get("checked_at")?.as_str()?.replace('Z', "+00:00"),
        )?;
        let age = now.as_nanosecond() - i128::from(checked_at.as_microsecond()) * 1000;
        Some(
            provenant_core::py::python_json_eq(
                probe.get("cli_version").unwrap_or(&serde_json::Value::Null),
                route.get("cli_version").unwrap_or(&serde_json::Value::Null),
            ) && provenant_core::py::python_truth(
                route.get("cli_version").unwrap_or(&serde_json::Value::Null),
            ) && (0..86_400_000_000_000i128).contains(&age)
                && probe.get("attempted_write")?.as_bool()?
                && probe.get("permission_denied")?.as_bool()?
                && !probe.get("file_created")?.as_bool()?,
        )
    })()
    .unwrap_or(false);
    if valid {
        "enforced"
    } else {
        "prompt_only"
    }
}

/// Checked JSON boundary for the version-bound probe policy.
pub fn read_only_guarantee_json(
    route: &[u8],
    now: jiff::Timestamp,
) -> Result<&'static str, crate::DecodeError> {
    let route: serde_json::Value = crate::decode_json(route)?;
    Ok(read_only_guarantee(&route, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn json_probe_boundary_refuses_distinct_oversized_integer_versions() {
        let now = "2026-10-23T12:00:00Z".parse().unwrap();
        let route = br#"{"cli_version":18446744073709551616,"read_only_probe":{"cli_version":18446744073709551617,"checked_at":"2026-10-23T11:00:00Z","attempted_write":true,"permission_denied":true,"file_created":false}}"#;
        assert!(matches!(
            read_only_guarantee_json(route, now),
            Err(crate::DecodeError::NumberOutOfRange)
        ));
        let valid = br#"{"cli_version":18446744073709551615,"read_only_probe":{"cli_version":18446744073709551615,"checked_at":"2026-10-23T11:00:00Z","attempted_write":true,"permission_denied":true,"file_created":false}}"#;
        assert_eq!(read_only_guarantee_json(valid, now).unwrap(), "enforced");
    }

    #[test]
    fn read_only_guarantee_matches_python_on_json_versions_and_iso_forms() {
        // Python supplies a microsecond clock value and uses its real clock in
        // the unchanged source. Every fixture is hours away from an age boundary.
        let code = r#"
import json
from datetime import UTC, datetime, timedelta
from skills.orchestrate.scripts.adapters.kiro import read_only_guarantee
now = datetime.now(UTC)
fresh = now - timedelta(hours=1)
versions = [None, False, True, 0, 1, 2, 2.0, "", "2", [], [2], {}, {"v": 2}, 9007199254740993, 9007199254740992.0, 18446744073709551615]
stamps = [fresh.isoformat(), fresh.strftime("%Y%m%dT%H%M%S.%fZ"), fresh.isoformat().replace("+00:00", "999999Z"), fresh.isoformat().replace(".", ","), (now-timedelta(days=2)).isoformat(), (now+timedelta(hours=1)).isoformat(), "bad", fresh.replace(tzinfo=None).isoformat()]
routes = []
for version in versions:
    for other in versions:
        for stamp in stamps:
            routes.append({"cli_version": version, "read_only_probe": {"cli_version": other, "checked_at": stamp, "attempted_write": True, "permission_denied": True, "file_created": False}})
for field in ["attempted_write", "permission_denied", "file_created", "checked_at", "cli_version"]:
    for value in [None, 0, 1, "", "true", [], {}]:
        route = {"cli_version": "2", "read_only_probe": {"cli_version": "2", "checked_at": fresh.isoformat(), "attempted_write": True, "permission_denied": True, "file_created": False}}
        route["read_only_probe"][field] = value
        routes.append(route)
routes += [{}, {"cli_version": "2"}]
for probe in [None, False, 0, "", "x", [], [1], {}]:
    routes.append({"cli_version": "2", "read_only_probe": probe})
print(json.dumps([now.isoformat(), routes, [read_only_guarantee(r) for r in routes]]))
"#;
        let Some((now, routes, expected)): Option<(String, Vec<serde_json::Value>, Vec<String>)> =
            crate::oracle::python(code, &serde_json::Value::Null, None)
        else {
            return;
        };
        let now = now.parse().unwrap();
        let actual: Vec<_> = routes
            .iter()
            .map(|route| read_only_guarantee(route, now))
            .collect();
        assert_eq!(actual.len(), expected.len());
        for ((route, actual), expected) in routes.iter().zip(actual).zip(expected) {
            assert_eq!(actual, expected, "{route}");
        }
    }

    #[test]
    fn arbitrary_iso_separator_matches_python() {
        // The shared owner accepts any single Unicode separator, as CPython does.
        let code = r#"
import json
from datetime import UTC, datetime, timedelta
from skills.orchestrate.scripts.adapters.kiro import read_only_guarantee
now = datetime.now(UTC)
fresh = now - timedelta(hours=1)
stamps = [fresh.isoformat().replace("T", "🦀")]
routes = [{"cli_version": 2, "read_only_probe": {"cli_version": 2, "checked_at": stamp, "attempted_write": True, "permission_denied": True, "file_created": False}} for stamp in stamps]
print(json.dumps([now.isoformat(), routes, [read_only_guarantee(r) for r in routes]]))
"#;
        let Some((now, routes, expected)): Option<(String, Vec<serde_json::Value>, Vec<String>)> =
            crate::oracle::python(code, &serde_json::Value::Null, None)
        else {
            return;
        };
        let now = now.parse().unwrap();
        assert_eq!(expected, ["enforced"]);
        for route in routes {
            assert_eq!(read_only_guarantee(&route, now), "enforced", "{route}");
        }
    }

    #[test]
    fn read_only_probe_accepts_json_versions_and_python_iso_precision() {
        let now = "2026-10-23T12:00:00.123456Z".parse().unwrap();
        for version in [
            serde_json::json!(2),
            serde_json::json!(true),
            serde_json::json!([2]),
            serde_json::json!({"v": 2}),
        ] {
            for stamp in ["2026-10-23T12:00:00.123456999Z", "20261023T120000.123456Z"] {
                let route = serde_json::json!({
                    "cli_version": version,
                    "read_only_probe": {
                        "cli_version": version, "checked_at": stamp,
                        "attempted_write": true, "permission_denied": true, "file_created": false
                    }
                });
                assert_eq!(read_only_guarantee(&route, now), "enforced", "{route}");
            }
        }
    }

    #[test]
    fn kiro_uses_v3_and_omits_explicit_auto_model() {
        let mut p = plan(Mode::ReadOnly);
        p.model = Some("auto".into());
        let adapter = kiro(&p).unwrap();
        assert!(adapter
            .argv
            .windows(2)
            .any(|v| v == ["--agent-engine", "v3"]));
        assert!(!adapter.argv.contains(&"--model".into()));
        assert_eq!(adapter.argv.last().unwrap(), "Read only\n\ndo it");
    }

    #[test]
    fn read_only_probe_requires_matching_cli_and_is_less_than_one_day_old() {
        let now: jiff::Timestamp = "2026-10-23T12:00:00Z".parse().unwrap();
        let route = serde_json::json!({
            "cli_version": "2.23.1",
            "read_only_probe": {
                "cli_version": "2.23.1",
                "checked_at": "2026-10-22T12:00:01Z",
                "attempted_write": true,
                "permission_denied": true,
                "file_created": false
            }
        });
        assert_eq!(read_only_guarantee(&route, now), "enforced");
        let stale = serde_json::json!({
            "cli_version": "2.23.1",
            "read_only_probe": {
                "cli_version": "2.23.1",
                "checked_at": "2026-10-22T12:00:00Z",
                "attempted_write": true,
                "permission_denied": true,
                "file_created": false
            }
        });
        assert_eq!(read_only_guarantee(&stale, now), "prompt_only");
    }
}
