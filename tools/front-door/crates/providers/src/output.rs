//! Provider stream interpretation, with explicit clock and catalogue seams.
pub use crate::output_contract::{ObservedModel, ParsedOutput};
pub use crate::output_policy::{
    Classifier, ModelCatalogue, OutputError, OutputEvents, error_class, model_unavailable_fix,
    quota_window_is_short, reset_time, variant_fix,
};
use crate::output_policy::{is_match, reset_time_nodes, truncate};
use crate::{
    adapters,
    ordered_json::{self, Node},
};
use provenant_core::py::{python_str, python_strip, python_truth};
use regex::{Regex, RegexBuilder};
use serde_json::Value;

pub struct OutputParser {
    classifier: Classifier,
}
impl OutputParser {
    pub fn new() -> Result<Self, OutputError> {
        Ok(Self {
            classifier: Classifier::new()?,
        })
    }
    pub fn parse(
        &self,
        adapter: &str,
        stdout: &str,
        stderr: &str,
        exit_code: i64,
        at: &jiff::Zoned,
    ) -> Result<ParsedOutput, OutputError> {
        let config = adapters::adapter(adapter)
            .ok_or_else(|| OutputError::UnknownAdapter(adapter.into()))?;
        let mut result = ParsedOutput::default();
        let (mut events, mut plain, mut parts, mut errors, mut retry_errors) = (
            Vec::<Node>::new(),
            Vec::<String>::new(),
            Vec::<String>::new(),
            Vec::<String>::new(),
            Vec::<String>::new(),
        );
        let mut duplicate = false;
        match ordered_json::parse(stdout) {
            Ok(whole) => {
                duplicate |= whole.duplicates();
                if whole.is_object() {
                    if whole.get("type").is_none()
                        && !(adapter == "agy" && whole.get("status").is_some())
                    {
                        plain.push(stdout.to_owned());
                    }
                    events.push(whole);
                }
            }
            Err(crate::DecodeError::NumberOutOfRange) => {
                return Err(crate::DecodeError::NumberOutOfRange.into());
            }
            Err(_) => {
                for line in crate::pending::python_splitlines(stdout, true) {
                    match ordered_json::parse(line) {
                        Ok(event) => {
                            duplicate |= event.duplicates();
                            if event.is_object() {
                                events.push(event);
                            }
                        }
                        Err(crate::DecodeError::NumberOutOfRange) => {
                            return Err(crate::DecodeError::NumberOutOfRange.into());
                        }
                        Err(_) => plain.push(line.to_owned()),
                    }
                }
            }
        }
        if adapter == "agy"
            && (duplicate
                || stdout.contains('\u{fffd}')
                || (!plain.is_empty() && events.iter().any(|e| e.get("status").is_some())))
        {
            errors.push("invalid agy envelope".into());
        }
        let (mut terminal_text, mut provider_message, mut partial) = (None, None, false);
        for event in &events {
            let mut kind = event.get("type").and_then(Node::string).unwrap_or("");
            if event
                .get("type")
                .is_some_and(|n| n.truth() && (n.is_array() || n.is_object()))
            {
                return Err(OutputError::InvalidEventKind);
            }
            if config.continues_turn(&event.value())? {
                parts.clear();
                result.terminal = false;
            }
            if !event.get("type").is_some_and(Node::truth) {
                kind = event.get("event").and_then(Node::string).unwrap_or("");
            }
            if adapter == "claude"
                && kind == "assistant"
                && event
                    .get("parent_tool_use_id")
                    .is_some_and(|v| !v.is_null())
            {
                continue;
            }
            if adapter == "kiro" && str_eq(event.get("jsonrpc"), "2.0") {
                for item in event.objects() {
                    if str_eq(item.get("sessionUpdate"), "agent_message_chunk") {
                        if let Some(text) = item
                            .get("content")
                            .and_then(|c| c.get("text"))
                            .and_then(Node::string)
                        {
                            parts.push(text.into());
                        }
                    }
                    if str_eq(item.get("category"), "model") {
                        if let Some(model) = item.get("currentValue").and_then(Node::string) {
                            result.observed_model = Some(model.into());
                        }
                    }
                }
                if event
                    .get("result")
                    .and_then(|r| r.get("stopReason"))
                    .is_some_and(Node::truth)
                {
                    result.terminal = true;
                }
                if let Some(error) = event.get("error").filter(|n| n.truth()) {
                    errors.push(error.dumps(true));
                }
            }
            if adapter == "kiro" && matches!(kind, "sessionUpdate" | "runFinished") {
                if let Some(data) = event.get("data").filter(|n| n.is_object()) {
                    if let Some(update) = data.get("update").filter(|n| n.is_object()) {
                        if str_eq(update.get("sessionUpdate"), "agent_message_chunk") {
                            if let Some(text) = update
                                .get("content")
                                .and_then(|n| n.get("text"))
                                .and_then(Node::string)
                            {
                                parts.push(text.into());
                            }
                        }
                    }
                    if kind == "runFinished" {
                        result.terminal = true;
                        terminal_text = data
                            .get("finalText")
                            .and_then(Node::string)
                            .map(str::to_owned)
                            .or(terminal_text);
                        if data
                            .get("status")
                            .map(Node::python_str)
                            .unwrap_or_else(|| "success".into())
                            .to_lowercase()
                            != "success"
                        {
                            errors.push(truncate(&data.dumps(false), 400));
                        }
                    }
                } else if kind == "runFinished" {
                    result.terminal = true;
                }
            }
            for (index, item) in event.objects().into_iter().enumerate() {
                if adapter != "claude" || index == 0 {
                    for key in config.capabilities().session_keys {
                        if let Some(session) = item.get(key).and_then(Node::string) {
                            result.session_id = Some(session.into());
                        }
                    }
                }
                if let Some(number) = item.get("retry_after").and_then(Node::number) {
                    result.retry_after = Some(number);
                }
            }
            if matches!(adapter, "claude" | "cursor" | "agy" | "kiro")
                && matches!(
                    kind,
                    "init" | "system" | "session_start" | "session.created" | "message_start"
                )
            {
                for item in event.objects() {
                    if let Some(model) = item
                        .get("model")
                        .and_then(Node::string)
                        .filter(|s| *s != "<synthetic>")
                    {
                        result.observed_model = Some(model.into());
                    }
                }
            }
            if adapter == "claude" && kind == "assistant" {
                if let Some(model) = event
                    .get("message")
                    .and_then(|n| n.get("model"))
                    .and_then(Node::string)
                    .filter(|s| *s != "<synthetic>")
                {
                    result.observed_model = Some(model.into());
                }
            }
            let mut error_event = matches!(
                kind,
                "error" | "turn.failed" | "api_retry" | "provider.quota" | "AGY_ERROR"
            ) || event.get("is_error").is_some_and(Node::truth);
            if kind == "rate_limit_event" {
                error_event = event.objects().iter().any(|item| {
                    matches!(
                        item.get("status").and_then(Node::string),
                        Some("rejected" | "limited" | "exceeded")
                    )
                });
                if error_event {
                    errors.push(format!("usage limit {}", event.dumps(true)));
                }
            }
            if error_event {
                for item in event.objects() {
                    let body = item.get("body");
                    let parsed_body = body
                        .and_then(Node::string)
                        .and_then(|s| ordered_json::parse(s).ok());
                    let body = parsed_body
                        .as_ref()
                        .or(body)
                        .filter(|n| n.is_object())
                        .unwrap_or(item);
                    for detail in body.objects() {
                        if let Some(message) = detail.get("message").and_then(Node::string) {
                            provider_message = Some(message.to_owned());
                        }
                    }
                }
                let error_text = event.dumps(false);
                if kind == "api_retry"
                    || (adapter == "codex"
                        && kind == "error"
                        && error_text.to_lowercase().contains("reconnecting"))
                {
                    retry_errors.push(error_text);
                } else {
                    errors.push(error_text);
                }
            }
            if let Some(denied) = event.get("denied_actions") {
                if !denied.is_null() && !denied.is_array() {
                    errors.push("invalid agy envelope: malformed denied actions field".into());
                }
                if denied.is_array() && denied.truth() {
                    errors.push(format!("denied_actions {}", denied.dumps(true)));
                }
            }
            if kind == "result" {
                if event.get("is_error").is_some_and(Node::is_false) {
                    retry_errors.clear();
                }
                result.terminal = true;
                if let Some(text) = event
                    .get("result")
                    .or_else(|| event.get("text"))
                    .or_else(|| event.get("response"))
                    .and_then(Node::string)
                {
                    terminal_text = Some(text.into());
                }
            }
            if matches!(
                kind,
                "turn.completed" | "turn.failed" | "session.completed" | "done"
            ) {
                result.terminal = true;
            }
            if adapter == "codex" && kind == "turn.completed" {
                retry_errors.clear();
            }
            if kind == "assistant" {
                if let Some(content) = event.get("message").and_then(|m| m.get("content")) {
                    if let Some(s) = content.string() {
                        parts.push(s.into());
                    } else if let Node::Array(items) = content {
                        for item in items {
                            if str_eq(item.get("type"), "text") {
                                if let Some(s) = item.get("text").and_then(Node::string) {
                                    parts.push(s.into());
                                }
                            }
                        }
                    }
                }
            }
            if kind == "item.completed" {
                if let Some(item) = event.get("item") {
                    if str_eq(item.get("type"), "agent_message") {
                        if let Some(s) = item.get("text").and_then(Node::string) {
                            parts.push(s.into());
                        }
                    }
                }
            }
            if matches!(kind, "text" | "text_delta" | "assistant_message") {
                let text = event
                    .get("part")
                    .filter(|p| p.is_object())
                    .and_then(|p| p.get("text"))
                    .or_else(|| event.get("text"));
                if let Some(s) = text.and_then(Node::string) {
                    parts.push(s.into());
                }
            }
            if kind == "step_finish"
                && str_eq(event.get("part").and_then(|p| p.get("reason")), "stop")
            {
                result.terminal = true;
            }
            let envelope = if adapter == "agy" {
                event
                    .get("result")
                    .filter(|n| n.is_object())
                    .unwrap_or(event)
            } else {
                event
            };
            if adapter == "agy"
                && envelope.get("status").is_some()
                && envelope.get("response").is_some()
            {
                let (Some(status), Some(response)) = (
                    envelope.get("status").and_then(Node::string),
                    envelope.get("response").and_then(Node::string),
                ) else {
                    errors.push("invalid agy envelope".into());
                    continue;
                };
                let error = envelope.get("error");
                result.terminal = true;
                terminal_text = Some(response.into());
                let has_error = error.is_some_and(Node::truth);
                if status.to_uppercase() == "SUCCESS" && exit_code != 0 && !has_error {
                    errors.push(format!("provider exited {exit_code} despite SUCCESS"));
                }
                if status.to_uppercase() != "SUCCESS" || has_error {
                    if error
                        .map(Node::python_str)
                        .unwrap_or_else(|| "None".into())
                        .to_lowercase()
                        .contains("timeout")
                        || matches!(status.to_uppercase().as_str(), "TIMEOUT" | "PARTIAL")
                    {
                        partial = !python_strip(response).is_empty();
                        if !partial {
                            errors.push("print-timeout".into());
                        }
                    } else {
                        errors.push(error.filter(|n|n.truth()).map(Node::python_str).unwrap_or_else(||"provider returned a non-success status without an error message".into()));
                    }
                }
            }
        }
        if adapter == "agy" {
            if result.observed_model.as_ref().is_none_or(String::is_empty) {
                if let Some(c) = RegexBuilder::new(r#"^\s*using model\b[=:]?\s*["']?([\w./-]+)"#)
                    .case_insensitive(true)
                    .multi_line(true)
                    .build()?
                    .captures(stderr)
                {
                    result.observed_model = c.get(1).map(|s| s.as_str().into());
                }
            }
            if stderr.contains("AGY_ERROR") {
                errors.push(stderr.into());
            }
            if is_match("print.?timeout", stderr)? {
                partial =
                    terminal_text.as_ref().is_some_and(|s| !s.is_empty()) || !parts.is_empty();
            }
        }
        result.text = terminal_text.unwrap_or_else(|| {
            if !parts.is_empty() {
                parts.join("\n")
            } else {
                plain.concat()
            }
        });
        errors.extend(retry_errors);
        let (diagnostics, warning) = config.diagnostics(stderr);
        let mut failure_text = if errors.is_empty() {
            diagnostics
        } else {
            errors.join("\n")
        };
        // Preserve source assignment before denial augmentation.
        result.failure_text = failure_text.clone();
        let denial = adapter == "agy"
            && is_match(
                r"permission denied|permission blocked|not allowed to|tool required the .+ permission|denied_actions",
                stderr,
            )?;
        if denial {
            errors.insert(0, stderr.into());
            failure_text = format!("{stderr}\n{failure_text}");
        }
        let status = if !errors.is_empty() || exit_code != 0 || denial {
            let signature = self.classifier.classify(adapter, &failure_text)?;
            result.signature = signature.map(str::to_owned);
            let status = match signature {
                Some("invalid_input") => "rejected",
                Some("provider_transport") => "failed",
                Some(s) => s,
                None => {
                    if failure_text.contains("print-timeout") {
                        "timed_out"
                    } else {
                        "failed"
                    }
                }
            };
            let joined = errors.join("\n");
            let excerpt = provider_message
                .as_deref()
                .filter(|s| !s.is_empty())
                .or_else(|| Some(python_strip(&joined)).filter(|s| !s.is_empty()))
                .or_else(|| Some(warning.as_str()).filter(|s| !s.is_empty()))
                .or_else(|| Some(python_strip(&failure_text)).filter(|s| !s.is_empty()))
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    config
                        .capabilities()
                        .empty_failure
                        .replace("{exit_code}", &exit_code.to_string())
                });
            result.excerpt = truncate(&excerpt, 200);
            status.to_owned()
        } else if partial {
            result.signature = Some("print_timeout".into());
            "partial".into()
        } else if adapter == "codex" && !parts.is_empty() && !result.terminal {
            result.signature = Some("provider_incomplete".into());
            result.excerpt = "provider exited without a terminal result".into();
            "failed".into()
        } else if !python_strip(&result.text).is_empty() {
            "ok".into()
        } else {
            result.signature = Some("empty_output".into());
            result.excerpt = truncate(&format!("no assistant text: {}", python_strip(stdout)), 200);
            "failed".into()
        };
        result.status = status;
        if config.capabilities().exit_after_output
            && result.status == "failed"
            && result.signature.is_none()
            && exit_code == 1
            && !parts.is_empty()
            && !python_strip(&result.text).is_empty()
            && errors.is_empty()
        {
            result.status = "ok".into();
            result.signature = Some("exit_after_output".into());
            result.excerpt = truncate(
                &if warning.is_empty() {
                    format!("provider exited {exit_code} after final assistant output")
                } else {
                    warning
                },
                200,
            );
        }
        if result.status == "ok" {
            if let Some(question) = terminal_question(&result.text)? {
                result.question = Some(question);
                result.status = "input_required".into();
            }
        }
        result.reset_at = reset_time_nodes(&failure_text, &events, at)?;
        Ok(result)
    }
}
fn str_eq(node: Option<&Node>, s: &str) -> bool {
    node.and_then(Node::string) == Some(s)
}
fn terminal_question(text: &str) -> Result<Option<String>, OutputError> {
    // Emulate the tempered-dot fence body without unsupported regex lookahead.
    let opener = Regex::new(r"\A(?:[^\n`]*\n)?\s*QUESTION:\s*")?;
    for (start, _) in text.match_indices("```") {
        let rest = &text[start + 3..];
        let Some(prefix) = opener.find(rest) else {
            continue;
        };
        let body = &rest[prefix.end()..];
        let Some(close) = body.find("```") else {
            continue;
        };
        if !body[close + 3..]
            .chars()
            .all(provenant_core::py::python_whitespace)
        {
            continue;
        }
        let question = python_strip(&body[..close]);
        if !question.is_empty() {
            return Ok(Some(truncate(question, 4096)));
        }
    }
    Ok(None)
}

pub fn same_model(
    adapter: &str,
    resolved: Option<&Value>,
    observed: Option<&Value>,
    catalogue: &dyn ModelCatalogue,
) -> Result<bool, OutputError> {
    let r = resolved.unwrap_or(&Value::Null);
    let o = observed.unwrap_or(&Value::Null);
    if !python_truth(r) || !python_truth(o) || provenant_core::py::python_json_eq(r, o) {
        return Ok(true);
    }
    let norm = |v: &Value| -> Result<String, regex::Error> {
        Ok(Regex::new(r"[\s_]+")?
            .replace_all(&python_strip(&python_str(v)).to_lowercase(), "-")
            .into_owned())
    };
    if python_strip(&python_str(o)).contains(' ') && norm(o)?.starts_with(&(norm(r)? + "-")) {
        return Ok(true);
    }
    if r.as_str().is_none() || o.as_str().is_none() {
        return Ok(false);
    }
    Ok(
        match (
            catalogue.registered_model(adapter, &python_str(r)),
            catalogue.registered_model(adapter, &python_str(o)),
        ) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_matches_source_golden_and_edge_fixtures() {
        let Some(cases): Option<Vec<(Value, Value)>> = crate::oracle::python(
            r#"
import json,sys
from pathlib import Path
from datetime import datetime
from skills.orchestrate.scripts import provider_output as m
at=datetime.fromisoformat('2026-10-23T01:00:00.123456+05:30')
cases=[]
for path in sorted(Path('tests/fixtures/fabric-v1').glob('provider-*.json')):
    f=json.loads(path.read_text()); cases.extend([f,*f.get('cases',[])])
def case(adapter,events=None,stderr='',exit=0,stdout=None):
    cases.append(dict(adapter=adapter,stdout=stdout if stdout is not None else '\n'.join(json.dumps(e,ensure_ascii=False) for e in (events or [])),stderr=stderr,exit=exit))
for adapter in ['claude','codex','opencode','cursor','agy','kiro','copilot']:
    for message in ['invalid model selection; quota exceeded per minute','RESOURCE_EXHAUSTED quota exceeded per day','RESOURCE_EXHAUSTED quota exceeded per minute','RESOURCE_EXHAUSTED PerMinute','RESOURCE_EXHAUSTED PerMinutePerDay','quota exceeded for tokenPerMinute. limit per day','quota exceeded per minute. Another limit per day','quota exceeded per minute\nper hour','usage limit','EPERM; Variant unavailable for p/m: high','Variant unavailable for p/m: high; provider.no-route','FreeTierError','you have hit your weekly limit','Individual quota reached','not logged in','permission denied','429','503','model at capacity','unknown model','403','reconnecting','some error']:
        case(adapter,stderr=message,exit=1)
    for output in ['{"type":1,"event":"result","result":"fake"}', '{"type":0,"event":"result","result":"real"}', '{"type":true,"event":"result","result":"fake"}', '{"type":[],"event":"result","result":"real"}', '', 'plain answer\n', '{}', '[]','3', '{"x":1,"x":2}', '{"type":"text","text":"first","text":"last"}', '{"type":"result","result":"ok"}\u2028{"type":"done"}\n', 'prefix\n{"type":"result","result":"ok"}', '{"type":"text","text":"�"}']:
        if adapter=='opencode' and '"type":[]' in output: continue
        case(adapter,stdout=output)
    for events in [[{'type':'result','is_error':False,'result':'DONE'}],[{'type':'assistant','message':{'content':[{'type':'text','text':'part1'},{'type':'tool_use','text':'no'}]}}],[{'type':'item.completed','item':{'type':'agent_message','text':'A'}}],[{'type':'text_delta','part':{'text':'delta'}}],[{'type':'text','part':{'text':None},'text':'fallback'}],[{'type':'error','error':{'message':'model unavailable'}}],[{'type':'text','text':'answer'},{'type':'step_start'},{'type':'text','part':{'text':'final'}},{'type':'step_finish','part':{'reason':'stop'}}],[{'type':'api_retry','error':'529 overloaded'},{'type':'result','is_error':False,'result':'ok'}],[{'type':'api_retry','error':'529 overloaded'},{'type':'turn.completed'},{'type':'text','text':'OK'}],[{'type':'text','retry_after':True,'text':'ok','nested':{'retry_after':2.0}}],[{'type':'result','result':None,'text':'fallback'}],[{'type':'text','text':'a','denied_actions':{}}],[{'type':'text','text':'a','denied_actions':['read']}],[{'type':'init','model':'root','nested':{'model':'nested'}}]]:
        case(adapter,events)
for body in ['```\nQUESTION: Which?\n```','```txt\n QUESTION: X\n```\n','```QUESTION: x```','Example:\n```\nQUESTION: X\n```\nEnd.','```\nQUESTION: \u001cWhich?\u001f\n```','```\nQUESTION: '+('x'*5000)+'\n```']:
    case('claude',[{'type':'result','result':body}])
case('claude',[{'type':'system','session_id':'root'},{'type':'assistant','parent_tool_use_id':'sub','message':{'model':'bad','content':'bad'},'session_id':'sub'},{'type':'result','result':'ok','nested':{'session_id':'other'}}])
for event in [{'jsonrpc':'2.0','nested':{'sessionUpdate':'agent_message_chunk','content':{'text':'ok'},'sessionId':'k'},'result':{'stopReason':'end_turn'}},{'type':'sessionUpdate','data':{'sessionId':'k','update':{'sessionUpdate':'agent_message_chunk','content':{'text':'ok'}}}},{'type':'runFinished','data':{'status':'failed','finalText':'tail','sessionId':'k'}},{'type':'runFinished','data':None},{'jsonrpc':'2.0','error':{'message':'denied','code':401}}]:
    case('kiro',[event])
for status,response,error in [('SUCCESS','ok',None),('SUCCESS','ok',False),('FAILED','',{'message':'quota exceeded'}),('TIMEOUT','partial',None),('PARTIAL','',None),('ERROR','',None),('SUCCESS',3,None),(3,'ok',None)]:
    for nested in [False,True]:
        event=dict(status=status,response=response,error=error)
        case('agy',[{'event':'result','result':event}] if nested else [event],exit=0 if status=='SUCCESS' else 1)
case('agy',[{'status':'SUCCESS','response':'ok'}],exit=3)
case('agy',[{'type':'text','text':'part'}],stderr='using model="chosen"\nprint_timeout')
case('agy',[{'status':'SUCCESS','response':'ok'}],stderr='permission denied')
for stderr in ['', 'info hi\nwarn caution\nerror TS2769 failed','model at capacity','AGY_ERROR permission denied']:
    case('opencode',[{'type':'text','text':'DONE'}],stderr=stderr,exit=1)
for text in ['Resets in 3 days','Resets in 167h','Resets in 2h 15m','try again at 2:00 AM','try again at 1:00 AM','try again at 12:00 PM','Resets in ١h']:
    case('codex',stderr=text,exit=1)
for epoch in [True,0,1.5,1770000000.1234567,-1.0000005,253402300800,9007199254740993]:
    case('claude',[{'type':'error','retry_after':3,'z':{'resetsAt':epoch},'a':{'resetsAt':100}}],exit=1)
for event in [{'type':'error','body':json.dumps({'error':{'message':'actionable unicode 🦀'}}),'error':'failed'},{'type':'rate_limit_event','rate_limit_info':{'status':'allowed','resetsAt':100}},{'type':'rate_limit_event','rate_limit_info':{'status':'rejected','resetsAt':100}}]:
    case('claude',[event])
print(json.dumps([(c,m.parse_output(c['adapter'],c['stdout'],c.get('stderr',''),c['exit'],at=at)) for c in cases]))
"#,
            &Value::Null,
            None,
        ) else {
            return;
        };
        let at = "2026-10-23T01:00:00.123456+05:30[+05:30]".parse().unwrap();
        let parser = OutputParser::new().unwrap();
        for (input, expected) in &cases {
            let actual = parser
                .parse(
                    input["adapter"].as_str().unwrap(),
                    input["stdout"].as_str().unwrap(),
                    input.get("stderr").and_then(Value::as_str).unwrap_or(""),
                    input["exit"].as_i64().unwrap(),
                    &at,
                )
                .unwrap();
            assert_eq!(serde_json::to_value(actual).unwrap(), *expected, "{input}");
        }
        eprintln!("{} exact output oracle fixtures", cases.len());
        let events =
            OutputEvents::from_json(r#"[{"z":{"resetsAt":2},"a":{"resetsAt":1}}]"#).unwrap();
        assert_eq!(
            reset_time("", &events, &at).unwrap().as_deref(),
            Some("1970-01-01T00:00:02Z")
        );
    }
}
