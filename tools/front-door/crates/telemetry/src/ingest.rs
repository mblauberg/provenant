//! Allowlisted native/OTLP metadata parsers and snapshot-bounded JSONL cursors.
//! Opening a verified, non-symlink file remains the supervisor's safe OS boundary.
use crate::{Error, Event, Numeric, Observation, Result, Sink, Usage, delivery, number, pending};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{BufRead, Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ParserState {
    #[serde(flatten)]
    pub sidecar: delivery::SidecarState,
    pub session: Option<String>,
    pub repository: Option<String>,
    pub account: Option<String>,
    pub revision: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<BTreeMap<String, Value>>,
    #[serde(rename = "_file", skip_serializing_if = "Option::is_none")]
    pub file: Option<FileIdentity>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FileIdentity(pub u64, pub u64, pub u64, pub String);
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cursor {
    pub source: String,
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub offset: u64,
    pub state: ParserState,
}
/// Snapshot taken by the verified-open owner, not rediscovered by path during ingestion.
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub mtime_ns: i64,
}
pub trait CursorStore {
    fn load(&mut self, source: &str, path: &Path) -> Result<Option<Cursor>>;
    fn clear_file(&mut self, source: &str, path: &Path) -> Result<()>;
    fn save(&mut self, cursor: Cursor) -> Result<()>;
}
fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}
fn preferred(account: Option<&str>, fallback: Option<&str>) -> Option<String> {
    account
        .filter(|s| !s.is_empty())
        .or(fallback)
        .map(str::to_owned)
}

pub fn claude_record(
    sink: &mut impl Sink,
    row: &Value,
    path: &Path,
    offset: u64,
    account: Option<&str>,
) -> Result<()> {
    delivery::compact(sink, "claude", row, None, path, offset)?;
    let message = &row["message"];
    let usage = &message["usage"];
    if row["type"] != "assistant"
        || !usage.is_object()
        || message["id"].as_str().is_none_or(str::is_empty)
    {
        return Ok(());
    }
    let reasoning = Numeric::from_value(&usage["output_tokens_details"]["thinking_tokens"]);
    let output = Numeric::from_value(&usage["output_tokens"]);
    sink.usage(
        Usage {
            source: "claude".into(),
            event_id: message["id"].as_str().unwrap_or("").into(),
            session: text(row, "sessionId"),
            adapter: Some("claude".into()),
            provider: Some("anthropic".into()),
            model: text(message, "model"),
            observed_model: text(message, "model"),
            effort: text(row, "perTurnEffort"),
            account: preferred(account, row["account"].as_str()),
            repository: text(row, "cwd"),
            revision: text(row, "gitRevision"),
            timestamp: text(row, "timestamp"),
            input_tokens: Numeric::from_value(&usage["input_tokens"]),
            cache_read_tokens: Numeric::from_value(&usage["cache_read_input_tokens"]),
            cache_write_tokens: Numeric::from_value(&usage["cache_creation_input_tokens"]),
            output_tokens: output
                .map(|o| {
                    o.subtract(reasoning.unwrap_or(Numeric::ZERO))
                        .map(Numeric::non_negative)
                })
                .transpose()?,
            reasoning_tokens: reasoning,
            usage_source: Some("message.usage".into()),
            ..Usage::default()
        },
        path,
    )
}
const CODEX_TOKENS: [&str; 6] = [
    "input_tokens",
    "cached_input_tokens",
    "cache_write_input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "total_tokens",
];
fn token_map(value: &Value) -> BTreeMap<String, Value> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, v)| CODEX_TOKENS.contains(&k.as_str()) && number(v).is_some())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
fn subtract(value: &Value, previous: Option<&Value>) -> Value {
    if let Some(n) = value.as_u64() {
        if let Some(p) = previous
            .and_then(Value::as_u64)
            .or(Some(0).filter(|_| previous.is_none()))
        {
            return json!(n.saturating_sub(p));
        }
    }
    json!(
        (value.as_f64().unwrap_or(0.0) - previous.and_then(Value::as_f64).unwrap_or(0.0)).max(0.0)
    )
}
pub fn codex_record(
    sink: &mut impl Sink,
    row: &Value,
    state: &mut ParserState,
    path: &Path,
    offset: u64,
    account: Option<&str>,
) -> Result<()> {
    delivery::compact(sink, "codex", row, state.session.as_deref(), path, offset)?;
    let payload = &row["payload"];
    match row["type"].as_str() {
        Some("session_meta") => {
            state.session = text(payload, "id")
                .filter(|s| !s.is_empty())
                .or_else(|| text(payload, "session_id"));
            state.repository = text(payload, "cwd");
            state.account = text(payload, "creator_account_id");
            state.revision = text(&payload["git"], "commit_hash");
        }
        Some("turn_context") => {
            state.model = text(payload, "model");
            state.effort = text(payload, "effort");
            state.repository = text(payload, "cwd")
                .filter(|s| !s.is_empty())
                .or_else(|| state.repository.clone());
        }
        Some("event_msg") if payload["type"] == "token_count" => {
            let info = &payload["info"];
            let delta;
            let event_id;
            let session = state
                .session
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            if info["total_token_usage"].is_object() {
                let total = token_map(&info["total_token_usage"]);
                let reset = total.iter().any(|(key, value)| {
                    Numeric::from_value(value).unwrap_or(Numeric::ZERO)
                        < state
                            .total
                            .as_ref()
                            .and_then(|p| p.get(key))
                            .and_then(Numeric::from_value)
                            .unwrap_or(Numeric::ZERO)
                });
                delta = total
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            subtract(
                                v,
                                if reset {
                                    None
                                } else {
                                    state.total.as_ref().and_then(|p| p.get(k))
                                },
                            ),
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                state.total = Some(total);
                event_id = pending::identity(&[json!(session), json!(offset)]);
            } else {
                delta = token_map(&info["last_token_usage"]);
                event_id = pending::identity(&[
                    json!(session),
                    row["timestamp"].clone(),
                    serde_json::to_value(&delta)?,
                ]);
            }
            let get = |k: &str| delta.get(k).and_then(Numeric::from_value);
            let cached = get("cached_input_tokens");
            let written = get("cache_write_input_tokens");
            let reasoning = get("reasoning_output_tokens");
            let owner = preferred(account, state.account.as_deref());
            if delta
                .values()
                .any(|v| number(v).is_some_and(|n| !n.is_zero()))
            {
                sink.usage(
                    Usage {
                        source: "codex".into(),
                        event_id,
                        session: state.session.clone(),
                        repository: state.repository.clone(),
                        account: owner.clone(),
                        revision: state.revision.clone(),
                        model: state.model.clone(),
                        observed_model: state.model.clone(),
                        effort: state.effort.clone(),
                        adapter: Some("codex".into()),
                        provider: Some("openai".into()),
                        timestamp: text(row, "timestamp"),
                        input_tokens: get("input_tokens")
                            .map(|n| {
                                n.subtract(cached.unwrap_or(Numeric::ZERO))?
                                    .subtract(written.unwrap_or(Numeric::ZERO))
                                    .map(Numeric::non_negative)
                            })
                            .transpose()?,
                        cache_read_tokens: cached,
                        cache_write_tokens: written,
                        output_tokens: get("output_tokens")
                            .map(|n| {
                                n.subtract(reasoning.unwrap_or(Numeric::ZERO))
                                    .map(Numeric::non_negative)
                            })
                            .transpose()?,
                        reasoning_tokens: reasoning,
                        usage_source: Some("token_count".into()),
                        ..Usage::default()
                    },
                    path,
                )?;
            }
            for (window, value) in payload["rate_limits"].as_object().into_iter().flatten() {
                let Some(used_percent) = number(&value["used_percent"]) else {
                    continue;
                };
                let reset_at = if let Some(seconds) = value["resets_at"].as_f64() {
                    Some(pending::iso(pending::timestamp_seconds(seconds)?))
                } else {
                    text(value, "resets_at")
                };
                sink.observation(Observation {
                    source: "codex".into(),
                    origin_path: path.to_string_lossy().into_owned(),
                    provider: Some("openai".into()),
                    adapter: Some("codex".into()),
                    model: state.model.clone(),
                    account: owner.clone(),
                    session: state.session.clone(),
                    timestamp: text(row, "timestamp"),
                    window: window.clone(),
                    used_percent,
                    window_minutes: number(&value["window_minutes"]),
                    reset_at,
                })?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub fn otel_attributes(items: &Value) -> Result<BTreeMap<String, Value>> {
    let mut values = BTreeMap::new();
    for item in items.as_array().into_iter().flatten() {
        let Some(key) = item["key"].as_str() else {
            continue;
        };
        let v = &item["value"];
        let value = if let Some(n) = v.get("intValue") {
            if n.is_string() {
                let parsed: Value = pending::decode_json(n.as_str().unwrap_or("").as_bytes())?;
                if parsed.as_i64().is_none() && parsed.as_u64().is_none() {
                    return Err(Error::Invalid("invalid OTLP intValue".into()));
                }
                parsed
            } else if n.as_i64().is_some() || n.as_u64().is_some() {
                n.clone()
            } else {
                return Err(Error::Invalid("invalid OTLP intValue".into()));
            }
        } else if let Some(n) = v.get("doubleValue") {
            let n = n
                .as_f64()
                .or_else(|| n.as_str().and_then(|s| s.parse().ok()))
                .ok_or_else(|| Error::Invalid("invalid OTLP doubleValue".into()))?;
            json!(n)
        } else if let Some(n) = v.get("boolValue") {
            json!(pending::python_truthy(n))
        } else if let Some(n) = v.get("stringValue") {
            n.clone()
        } else {
            continue;
        };
        values.insert(key.into(), value);
    }
    Ok(values)
}
pub fn otel_stamp(nanos: &Value, fallback: Option<&str>) -> Option<String> {
    let nanos = nanos
        .as_str()
        .and_then(|s| s.parse::<i128>().ok())
        .or_else(|| nanos.as_i64().map(i128::from));
    nanos
        .and_then(|n| pending::timestamp_seconds(n as f64 / 1e9).ok())
        .map(pending::iso)
        .or_else(|| fallback.map(str::to_owned))
}
const STRING_ATTRS: [&str; 5] = ["model", "effort", "type", "tool_name", "decision"];
const NUMERIC_ATTRS: [&str; 16] = [
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
    "cache_creation_5m_tokens",
    "cache_creation_1h_tokens",
    "cost_usd",
    "cost_usd_micros",
    "duration_ms",
    "ttft_ms",
    "prompt_length",
    "response_length",
    "tool_input_size_bytes",
    "tool_result_size_bytes",
    "attempt",
    "http.response.status_code",
];
pub fn otel_record(sink: &mut impl Sink, row: &Value, path: &Path, offset: u64) -> Result<()> {
    let logs = row["signal"] == "logs";
    let metrics = row["signal"] == "metrics";
    let resources = &row["body"][if logs {
        "resourceLogs"
    } else if metrics {
        "resourceMetrics"
    } else {
        ""
    }];
    let mut index = 0;
    for resource in resources.as_array().into_iter().flatten() {
        let context = otel_attributes(&resource["resource"]["attributes"])?;
        for scope in resource[if logs { "scopeLogs" } else { "scopeMetrics" }]
            .as_array()
            .into_iter()
            .flatten()
        {
            for record in scope[if logs { "logRecords" } else { "metrics" }]
                .as_array()
                .into_iter()
                .flatten()
            {
                let points = if logs {
                    vec![record]
                } else {
                    record
                        .get("sum")
                        .filter(|v| v.as_object().is_some_and(|v| !v.is_empty()))
                        .unwrap_or(&record["gauge"])["dataPoints"]
                        .as_array()
                        .map(|a| a.iter().collect())
                        .unwrap_or_default()
                };
                for point in points {
                    index += 1;
                    let attrs = otel_attributes(&point["attributes"])?;
                    let name = attrs
                        .get("event.name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("log")
                        .to_lowercase();
                    if logs
                        && ![
                            "api_request",
                            "codex.api_request",
                            "user_prompt",
                            "tool_decision",
                            "codex.user_prompt",
                            "codex.tool_decision",
                        ]
                        .contains(&name.as_str())
                    {
                        continue;
                    }
                    let mut kept = serde_json::Map::new();
                    for (key, value) in &attrs {
                        let allowed = if logs
                            && matches!(name.as_str(), "user_prompt" | "codex.user_prompt")
                        {
                            key == "prompt_length"
                        } else if logs
                            && matches!(name.as_str(), "tool_decision" | "codex.tool_decision")
                        {
                            key == "tool_name" || key == "decision"
                        } else {
                            STRING_ATTRS.contains(&key.as_str())
                                || key == "success"
                                || NUMERIC_ATTRS.contains(&key.as_str())
                        };
                        let typed = STRING_ATTRS.contains(&key.as_str()) && value.is_string()
                            || key == "success" && value.is_boolean()
                            || NUMERIC_ATTRS.contains(&key.as_str())
                                && (value.is_number() || value.is_boolean());
                        if allowed && typed {
                            kept.insert(key.clone(), value.clone());
                        }
                    }
                    if !logs {
                        let v = point.get("asInt").unwrap_or(&point["asDouble"]);
                        let value = if v.is_null() {
                            Value::Null
                        } else {
                            json!(
                                v.as_f64()
                                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                                    .ok_or_else(|| Error::Invalid(
                                        "invalid OTLP metric value".into()
                                    ))?
                            )
                        };
                        kept.insert("name".into(), record["name"].clone());
                        kept.insert("value".into(), value);
                        kept.insert(
                            "temporality".into(),
                            record["sum"]["aggregationTemporality"].clone(),
                        );
                    }
                    let get = |m: &BTreeMap<String, Value>, k: &str| {
                        m.get(k).and_then(Value::as_str).map(str::to_owned)
                    };
                    kept.retain(|_, value| !value.is_null());
                    sink.event(Event {
                        kind: if logs {
                            format!("otel.{name}")
                        } else {
                            "otel.metric".into()
                        },
                        ts: otel_stamp(&point["timeUnixNano"], row["received_at"].as_str()),
                        source_ref: format!("{}@{offset}#{index}", path.display()),
                        attrs: Value::Object(kept),
                        session: get(&attrs, "session.id"),
                        model: get(&attrs, "model"),
                        run_id: get(&context, "provenant.run_id"),
                        lane: get(&context, "provenant.lane"),
                        harness_rev: get(&context, "provenant.harness_rev"),
                        ..Event::default()
                    })?;
                }
            }
        }
    }
    Ok(())
}
pub fn file_identity(
    stream: &mut (impl Read + Seek),
    stat: Snapshot,
    length: u64,
) -> Result<FileIdentity> {
    stream.rewind().map_err(|e| Error::Io {
        path: "<verified transcript>".into(),
        source: e,
    })?;
    let mut digest = Sha256::new();
    let mut remaining = length;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let bound = remaining.min(buffer.len() as u64) as usize;
        let count = stream.read(&mut buffer[..bound]).map_err(|e| Error::Io {
            path: "<verified transcript>".into(),
            source: e,
        })?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(FileIdentity(
        stat.device,
        stat.inode,
        length,
        pending::hex(&digest.finalize()),
    ))
}
/// Only complete lines from the opened snapshot are parsed. A rewritten prefix clears its rows.
pub fn ingest_stream(
    store: &mut impl CursorStore,
    source: &str,
    path: &Path,
    stream: &mut (impl BufRead + Seek),
    stat: Snapshot,
    mut parse: impl FnMut(&Value, &mut ParserState, u64) -> Result<()>,
) -> Result<()> {
    let old = store.load(source, path)?;
    let mut state = old.as_ref().map(|c| c.state.clone()).unwrap_or_default();
    let previous = state.file.clone();
    if old
        .as_ref()
        .is_some_and(|c| c.size == stat.size && c.mtime == stat.mtime_ns)
        && previous
            .as_ref()
            .is_some_and(|p| (p.0, p.1) == (stat.device, stat.inode))
    {
        return Ok(());
    }
    let rewritten = if let Some(old) = &old {
        stat.size < old.size
            || if let Some(p) = &previous {
                p.2 < old.size || file_identity(stream, stat, p.2)? != *p
            } else {
                false
            }
    } else {
        false
    };
    if rewritten {
        store.clear_file(source, path)?;
        state = ParserState::default();
    }
    let mut offset = old
        .as_ref()
        .filter(|_| !rewritten)
        .map(|c| c.offset)
        .unwrap_or(0);
    stream
        .seek(SeekFrom::Start(offset))
        .map_err(|source| Error::Io {
            path: path.into(),
            source,
        })?;
    while offset < stat.size {
        let mut line = Vec::new();
        let read = stream
            .take((stat.size - offset).min((crate::MAX_JSON_BYTES + 1) as u64))
            .read_until(b'\n', &mut line)
            .map_err(|source| Error::Io {
                path: path.into(),
                source,
            })?;
        if line.len() > crate::MAX_JSON_BYTES {
            return Err(Error::InputTooLarge {
                limit: crate::MAX_JSON_BYTES,
            });
        }
        if !line.ends_with(b"\n") {
            break;
        }
        let row = pending::decode_json(&line)?;
        parse(&row, &mut state, offset)?;
        offset += read as u64;
    }
    state.file = Some(file_identity(stream, stat, stat.size)?);
    store.save(Cursor {
        source: source.into(),
        path: path.to_string_lossy().into_owned(),
        size: stat.size,
        mtime: stat.mtime_ns,
        offset,
        state,
    })
}
pub fn stamp_ms(value: &str) -> Option<f64> {
    crate::parse_time(value)
        .ok()
        .map(|t| t.as_nanosecond() as f64 / 1_000_000.0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[derive(Default)]
    pub(crate) struct Capture {
        pub usage: Vec<Usage>,
        pub events: Vec<Event>,
        pub observations: Vec<Observation>,
        pub updates: Vec<(String, String, Value)>,
    }
    impl Sink for Capture {
        fn usage(&mut self, r: Usage, _: &Path) -> Result<()> {
            self.usage.push(r);
            Ok(())
        }
        fn observation(&mut self, r: Observation) -> Result<()> {
            self.observations.push(r);
            Ok(())
        }
        fn event(&mut self, r: Event) -> Result<()> {
            self.events.push(r);
            Ok(())
        }
        fn update_event(&mut self, k: &str, s: &str, a: Value) -> Result<()> {
            self.updates.push((k.into(), s.into(), a));
            Ok(())
        }
    }
    #[test]
    fn claude_buckets_are_disjoint_and_no_content_survives() {
        let mut sink = Capture::default();
        claude_record(&mut sink,&json!({"type":"assistant","message":{"id":"m","model":"observed","content":"secret","usage":{"input_tokens":12,"output_tokens":10,"output_tokens_details":{"thinking_tokens":3}}}}),Path::new("fixture"),0,None).unwrap();
        assert_eq!(sink.usage[0].output_tokens, Some(7.into()));
        assert_eq!(sink.usage[0].reasoning_tokens, Some(3.into()));
        assert!(
            !serde_json::to_string(&sink.usage)
                .unwrap()
                .contains("secret")
        );
    }
    #[test]
    fn codex_cumulative_reset_and_compaction() {
        let mut sink = Capture::default();
        let mut state = ParserState::default();
        let path = Path::new("fixture");
        for row in [
            json!({"type":"session_meta","payload":{"id":"session","cwd":"repo"}}),
            json!({"type":"turn_context","payload":{"model":"observed","effort":"high"}}),
        ] {
            codex_record(&mut sink, &row, &mut state, path, 0, None).unwrap();
        }
        for (offset, total) in [(1, 100), (2, 120), (3, 10)] {
            codex_record(&mut sink,&json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":total,"cached_input_tokens":2,"output_tokens":5,"reasoning_output_tokens":1}}}}),&mut state,path,offset,None).unwrap();
        }
        assert_eq!(
            sink.usage
                .iter()
                .map(|u| u.input_tokens)
                .collect::<Vec<_>>(),
            vec![Some(98.into()), Some(20.into()), Some(8.into())]
        );
        codex_record(
            &mut sink,
            &json!({"type":"compacted","payload":{"secret":"content"}}),
            &mut state,
            path,
            4,
            None,
        )
        .unwrap();
        assert_eq!(sink.events[0].attrs, json!({}));
        assert_eq!(sink.events[0].session.as_deref(), Some("session"));
    }
    #[test]
    fn otlp_privacy_and_multiplicity() {
        let attributes = json!([{"key":"event.name","value":{"stringValue":"user_prompt"}},{"key":"prompt_length","value":{"intValue":"23"}},{"key":"prompt","value":{"stringValue":"secret"}}]);
        let record = json!({"attributes":attributes});
        let mut sink = Capture::default();
        otel_record(&mut sink,&json!({"signal":"logs","body":{"resourceLogs":[{"scopeLogs":[{"logRecords":[record.clone(),record]}]}]}}),Path::new("fixture"),5).unwrap();
        assert_eq!(sink.events.len(), 2);
        assert_ne!(sink.events[0].source_ref, sink.events[1].source_ref);
        assert_eq!(sink.events[0].attrs, json!({"prompt_length":23}));
    }
    #[derive(Default)]
    struct MemoryCursor {
        saved: Option<Cursor>,
        clears: usize,
    }
    impl CursorStore for MemoryCursor {
        fn load(&mut self, _: &str, _: &Path) -> Result<Option<Cursor>> {
            Ok(self.saved.clone())
        }
        fn clear_file(&mut self, _: &str, _: &Path) -> Result<()> {
            self.clears += 1;
            Ok(())
        }
        fn save(&mut self, c: Cursor) -> Result<()> {
            self.saved = Some(c);
            Ok(())
        }
    }
    #[test]
    fn partial_line_resumes_and_rewrite_reconciles() {
        let mut store = MemoryCursor::default();
        let mut parsed = Vec::new();
        let path = Path::new("fixture");
        for (mtime, bytes) in [
            (1, b"{\"x\":1}\n{\"x\":".as_slice()),
            (2, b"{\"x\":1}\n{\"x\":2}\n".as_slice()),
            (3, b"{\"x\":3}\n{\"x\":2}\n".as_slice()),
        ] {
            let mut stream = std::io::Cursor::new(bytes);
            let stat = Snapshot {
                device: 1,
                inode: 2,
                size: bytes.len() as u64,
                mtime_ns: mtime,
            };
            ingest_stream(&mut store, "test", path, &mut stream, stat, |r, _, _| {
                parsed.push(r["x"].clone());
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(parsed, vec![json!(1), json!(2), json!(3), json!(2)]);
        assert_eq!(store.clears, 1);
    }

    #[test]
    fn review_regression_observations_keep_integer_spelling() {
        let mut sink = Capture::default();
        codex_record(&mut sink, &json!({"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":0,"window_minutes":300}}}}), &mut ParserState::default(), Path::new("fixture"), 0, None).unwrap();
        let value = serde_json::to_value(&sink.observations[0]).unwrap();
        assert_eq!(value["used_percent"].to_string(), "0");
        assert_eq!(value["window_minutes"].to_string(), "300");
    }
    #[test]
    fn review_regression_oversized_integer_is_a_typed_error() {
        for text in [
            "{\"n\":18446744073709551616}\n",
            "{\"n\":-9223372036854775809}\n",
        ] {
            let mut stream = std::io::Cursor::new(text.as_bytes());
            let result = ingest_stream(
                &mut MemoryCursor::default(),
                "test",
                Path::new("fixture"),
                &mut stream,
                Snapshot {
                    device: 1,
                    inode: 2,
                    size: text.len() as u64,
                    mtime_ns: 1,
                },
                |_, _, _| Ok(()),
            );
            assert!(result.is_err(), "integer was silently rounded");
            assert!(matches!(result, Err(Error::NumberOutOfRange)));
        }
    }

    #[test]
    fn review_regression_timestamp_fraction_rounds_like_cpython() {
        let n = 1735689600791625390i128;
        assert_eq!(
            otel_stamp(&json!(n.to_string()), None).as_deref(),
            Some("2025-01-01T00:00:00.791625Z")
        );
        let mut sink = Capture::default();
        codex_record(&mut sink, &json!({"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":0,"resets_at":n as f64 / 1e9}}}}), &mut ParserState::default(), Path::new("fixture"), 0, None).unwrap();
        assert_eq!(
            sink.observations[0].reset_at.as_deref(),
            Some("2025-01-01T00:00:00.791625Z")
        );
        for (seconds, expected) in [
            (0.0000005, "1970-01-01T00:00:00Z"),
            (0.0000015, "1970-01-01T00:00:00.000002Z"),
            (-0.0000015, "1969-12-31T23:59:59.999998Z"),
            (0.9999996, "1970-01-01T00:00:01Z"),
        ] {
            assert_eq!(
                pending::iso(pending::timestamp_seconds(seconds).unwrap()),
                expected
            );
        }
    }
    #[test]
    fn review_regression_huge_timestamp_is_an_error() {
        let mut sink = Capture::default();
        let result = codex_record(
            &mut sink,
            &json!({"type":"event_msg","payload":{"type":"token_count","rate_limits":{"primary":{"used_percent":0,"resets_at":1e30}}}}),
            &mut ParserState::default(),
            Path::new("fixture"),
            0,
            None,
        );
        assert!(matches!(result, Err(Error::Timestamp(_))));
        for seconds in [
            f64::MAX,
            -f64::MAX,
            f64::INFINITY,
            f64::NAN,
            253_402_300_800.0,
            -62_135_596_801.0,
        ] {
            assert!(matches!(
                pending::timestamp_seconds(seconds),
                Err(Error::Timestamp(_))
            ));
        }
    }

    #[test]
    fn review_regression_cursor_keeps_python_previous() {
        let previous = json!({"source_ref":"repairs#0","attrs":{"repair_cycle":1}});
        let mut cursor: Cursor = serde_json::from_value(json!({"source":"delivery","path":"repairs.jsonl","size":0,"mtime":1,"offset":0,"state":{"previous":previous}})).unwrap();
        assert_eq!(
            serde_json::to_value(&cursor).unwrap()["state"]["previous"],
            previous
        );
        let mut sink = Capture::default();
        delivery::record(
            &mut sink,
            "repairs.jsonl",
            &json!({"cycle":2,"failing_before":[]}),
            &mut cursor.state.sidecar,
            &json!({}),
            "repairs#1".into(),
        )
        .unwrap();
        assert_eq!(sink.updates[0].1, "repairs#0");
    }
    #[test]
    fn review_regression_cursor_bounds_one_json_line() {
        let bytes = format!("{{\"x\":\"{}\"}}\n", "x".repeat(8 * 1024 * 1024));
        let mut stream = std::io::Cursor::new(bytes.as_bytes());
        let result = ingest_stream(
            &mut MemoryCursor::default(),
            "test",
            Path::new("fixture"),
            &mut stream,
            Snapshot {
                device: 1,
                inode: 2,
                size: bytes.len() as u64,
                mtime_ns: 1,
            },
            |_, _, _| Ok(()),
        );
        assert!(result.is_err(), "oversized line was accepted");
    }
}
