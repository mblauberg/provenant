//! Native hook renderers and exact selector reconciliation.
use crate::{HookRegistration, InstallError, Targets, managed::SelectorChange, splice};
use provenant_records::{
    ADAPTER_SPECS, Harness, HookAction, HookEvent, HookProtocol, HookTimeoutUnit, Limits,
    decode_jsonc,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, io::Cursor, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HookIdentity {
    harness: String,
    action: String,
    event: String,
    matcher: Option<String>,
    command: String,
}

fn registrations_for(harnesses: &[Harness], lane: bool) -> Vec<HookRegistration> {
    harnesses
        .iter()
        .filter_map(|harness| {
            let spec = harness.spec();
            let install = spec.install?;
            if !install.hooks.is_supported() {
                return None;
            }
            let protocol = install.hook_protocol?;
            Some(
                install
                    .hook_events
                    .iter()
                    .filter_map(|event| {
                        let HookEvent::Supported {
                            action,
                            event,
                            matcher,
                            timeout_seconds,
                        } = event
                        else {
                            return None;
                        };
                        // Shell and tool policy belongs to the outer lane
                        // confinement. The answer-delivery guard abstains in a
                        // lane, so registering it there adds no capability.
                        if lane && *action == HookAction::PreToolUse {
                            return None;
                        }
                        if *action == HookAction::PreToolUse
                            && !install.answer_delivery.is_supported()
                        {
                            return None;
                        }
                        if matches!(
                            action,
                            HookAction::ChairPrompt
                                | HookAction::ChairStart
                                | HookAction::ChairStop
                        ) && !spec.chair_switch
                        {
                            return None;
                        }
                        Some(HookRegistration {
                            harness: *harness,
                            action: *action,
                            protocol,
                            event,
                            matcher: *matcher,
                            timeout_seconds: *timeout_seconds,
                        })
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect()
}
pub fn registrations(harnesses: &[Harness]) -> Vec<HookRegistration> {
    registrations_for(harnesses, false)
}

fn hook_command(
    spec: &HookRegistration,
    binary: &Path,
    state: &Path,
) -> Result<String, InstallError> {
    let binary = binary
        .to_str()
        .ok_or(InstallError::InvalidConfig("hook executable must be UTF-8"))?;
    let state = state
        .to_str()
        .ok_or(InstallError::InvalidConfig("hook state root must be UTF-8"))?;
    let adapter = spec.harness.spec().name;
    Ok([
        binary,
        "--state-dir",
        state,
        "hook",
        adapter,
        spec.action.command(),
    ]
    .into_iter()
    .map(quote)
    .collect::<Vec<_>>()
    .join(" "))
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn hook_value(spec: &HookRegistration, binary: &Path, state: &Path) -> Result<Value, InstallError> {
    let mut value = json!({"type":"command", "command":hook_command(spec, binary, state)?});
    let timeout = spec.timeout_seconds.map(|seconds| match spec.protocol {
        HookProtocol::CursorCommands {
            timeout_unit: HookTimeoutUnit::Milliseconds,
        } => seconds.saturating_mul(1000),
        HookProtocol::CommandGroups {
            timeout_unit: HookTimeoutUnit::Milliseconds,
        }
        | HookProtocol::NamedGroups {
            timeout_unit: HookTimeoutUnit::Milliseconds,
        } => seconds.saturating_mul(1000),
        HookProtocol::CommandGroups {
            timeout_unit: HookTimeoutUnit::Seconds,
        }
        | HookProtocol::CursorCommands {
            timeout_unit: HookTimeoutUnit::Seconds,
        }
        | HookProtocol::NamedGroups {
            timeout_unit: HookTimeoutUnit::Seconds,
        }
        | HookProtocol::OpenCodePlugin => seconds,
    });
    if let Some(timeout) = timeout {
        value["timeout"] = timeout.into();
    }
    Ok(value)
}
pub(crate) fn value(spec: &HookRegistration, targets: &Targets) -> Result<Value, InstallError> {
    hook_value(
        spec,
        &targets.bin_dir.join("provenant"),
        targets.roots.state.as_path(),
    )
}

fn identity(spec: &HookRegistration, command: &str) -> HookIdentity {
    HookIdentity {
        harness: spec.harness.spec().name.into(),
        action: spec.action.command().into(),
        event: spec.event.into(),
        matcher: spec.matcher.map(str::to_owned),
        command: command.into(),
    }
}
pub(crate) fn selector(spec: &HookRegistration, command: &str) -> Result<String, InstallError> {
    Ok(format!(
        "hook:{}",
        serde_json::to_string(&identity(spec, command))
            .map_err(|_| InstallError::InvalidConfig("hook selector"))?
    ))
}
#[cfg(test)]
pub(crate) fn selector_with_command(selector: &str, command: &str) -> Result<String, InstallError> {
    let mut identity = read_identity(selector)?;
    identity.command = command.into();
    Ok(format!(
        "hook:{}",
        serde_json::to_string(&identity)
            .map_err(|_| InstallError::InvalidConfig("hook selector"))?
    ))
}
fn read_identity(selector: &str) -> Result<HookIdentity, InstallError> {
    let identity = selector
        .strip_prefix("hook:")
        .ok_or(InstallError::InvalidConfig("hook selector"))?;
    serde_json::from_str(identity).map_err(|_| InstallError::InvalidConfig("hook selector"))
}
fn harness(identity: &HookIdentity) -> Result<Harness, InstallError> {
    ADAPTER_SPECS
        .iter()
        .find(|spec| spec.name == identity.harness)
        .map(|spec| spec.id)
        .ok_or(InstallError::InvalidConfig("hook adapter"))
}
pub(crate) fn belongs_to(selector: &str, expected: Harness) -> Result<bool, InstallError> {
    Ok(harness(&read_identity(selector)?)? == expected)
}
fn action(identity: &HookIdentity) -> Result<HookAction, InstallError> {
    HookAction::ALL
        .iter()
        .copied()
        .find(|action| action.command() == identity.action)
        .ok_or(InstallError::InvalidConfig("hook action"))
}
fn protocol(identity: &HookIdentity) -> Result<HookProtocol, InstallError> {
    harness(identity)?
        .spec()
        .install
        .and_then(|install| install.hook_protocol)
        .ok_or(InstallError::InvalidConfig("hook protocol"))
}

/// Resolve the unique exact hook leaf. Selector identity carries the harness
/// protocol so unrelated registrations and user settings remain untouched.
pub(crate) fn path(bytes: &[u8], selector: &str) -> Result<Option<Vec<String>>, InstallError> {
    let identity = read_identity(selector)?;
    let protocol = protocol(&identity)?;
    let root: Value = decode_jsonc(Cursor::new(bytes), Limits::CONFIG)?;
    let action = action(&identity)?;
    let root_name = format!("provenant-{}", action.command());
    let mut found = None;
    let mut count = 0;
    let events = match protocol {
        HookProtocol::NamedGroups { .. } => root
            .get(&root_name)
            .and_then(|hooks| hooks.get(&identity.event))
            .and_then(Value::as_array),
        HookProtocol::CommandGroups { .. } | HookProtocol::CursorCommands { .. } => root
            .get("hooks")
            .and_then(|hooks| hooks.get(&identity.event))
            .and_then(Value::as_array),
        HookProtocol::OpenCodePlugin => None,
    };
    for (index, entry) in events.into_iter().flatten().enumerate() {
        match protocol {
            HookProtocol::CursorCommands { .. } => {
                if entry.get("matcher").and_then(Value::as_str) != identity.matcher.as_deref()
                    || entry.get("command").and_then(Value::as_str)
                        != Some(identity.command.as_str())
                {
                    continue;
                }
                count += 1;
                found = Some(vec![
                    "hooks".into(),
                    identity.event.clone(),
                    index.to_string(),
                ]);
            }
            HookProtocol::CommandGroups { .. } | HookProtocol::NamedGroups { .. } => {
                let mut prefix = if matches!(protocol, HookProtocol::NamedGroups { .. }) {
                    vec![root_name.clone(), identity.event.clone(), index.to_string()]
                } else {
                    vec!["hooks".into(), identity.event.clone(), index.to_string()]
                };
                if matches!(protocol, HookProtocol::NamedGroups { .. })
                    && identity.matcher.is_none()
                {
                    if entry.get("command").and_then(Value::as_str)
                        == Some(identity.command.as_str())
                    {
                        count += 1;
                        found = Some(prefix);
                    }
                    continue;
                }
                if entry.get("matcher").and_then(Value::as_str) != identity.matcher.as_deref() {
                    continue;
                }
                for (handler, leaf) in entry
                    .get("hooks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    if leaf.get("command").and_then(Value::as_str)
                        == Some(identity.command.as_str())
                    {
                        prefix.extend(["hooks".into(), handler.to_string()]);
                        count += 1;
                        found = Some(prefix.clone());
                        prefix.truncate(3);
                    }
                }
            }
            HookProtocol::OpenCodePlugin => {}
        }
    }
    if count > 1 {
        return Err(InstallError::InvalidConfig("duplicate owned hook"));
    }
    Ok(found)
}
pub(crate) fn get(bytes: &[u8], selector: &str) -> Result<Option<Value>, InstallError> {
    match path(bytes, selector)? {
        Some(path) => splice::get(bytes, &path.iter().map(String::as_str).collect::<Vec<_>>()),
        None => Ok(None),
    }
}
pub(crate) fn set(
    bytes: &[u8],
    selector: &str,
    value: Option<&Value>,
) -> Result<Vec<u8>, InstallError> {
    let identity = read_identity(selector)?;
    let protocol = protocol(&identity)?;
    let Some(path) = path(bytes, selector)? else {
        return Ok(bytes.to_vec());
    };
    let path = path.iter().map(String::as_str).collect::<Vec<_>>();
    let mut out = splice::set(bytes, &path, value)?;
    if value.is_none() {
        let action = action(&identity)?;
        let root_name = format!("provenant-{}", action.command());
        match protocol {
            HookProtocol::CommandGroups { .. } => {
                let group_path = &path[..3];
                if let Some(Value::Object(group)) = splice::get(&out, group_path)?
                    && group
                        .keys()
                        .all(|key| matches!(key.as_str(), "matcher" | "hooks"))
                    && group
                        .get("hooks")
                        .and_then(Value::as_array)
                        .is_some_and(Vec::is_empty)
                {
                    out = splice::set(&out, group_path, None)?;
                    let event_path = &path[..2];
                    if splice::get(&out, event_path)?
                        .and_then(|v| v.as_array().cloned())
                        .is_some_and(|v| v.is_empty())
                    {
                        out = splice::set(&out, event_path, None)?;
                    }
                    if splice::get(&out, &["hooks"])?
                        .and_then(|v| v.as_object().cloned())
                        .is_some_and(|v| v.is_empty())
                    {
                        out = splice::set(&out, &["hooks"], None)?;
                    }
                }
            }
            HookProtocol::NamedGroups { .. } => {
                if identity.matcher.is_some() {
                    let group_path = &path[..3];
                    if let Some(Value::Object(group)) = splice::get(&out, group_path)?
                        && group
                            .keys()
                            .all(|key| matches!(key.as_str(), "matcher" | "hooks"))
                        && group
                            .get("hooks")
                            .and_then(Value::as_array)
                            .is_some_and(Vec::is_empty)
                    {
                        out = splice::set(&out, group_path, None)?;
                    }
                }
                let event_path = &path[..2];
                if splice::get(&out, event_path)?
                    .and_then(|v| v.as_array().cloned())
                    .is_some_and(|v| v.is_empty())
                {
                    out = splice::set(&out, event_path, None)?;
                }
                if splice::get(&out, &[&root_name])?
                    .and_then(|v| v.as_object().cloned())
                    .is_some_and(|v| v.is_empty())
                {
                    out = splice::set(&out, &[&root_name], None)?;
                }
            }
            HookProtocol::CursorCommands { .. } => {
                let event_path = &path[..2];
                if splice::get(&out, event_path)?
                    .and_then(|v| v.as_array().cloned())
                    .is_some_and(|v| v.is_empty())
                {
                    out = splice::set(&out, event_path, None)?;
                }
            }
            HookProtocol::OpenCodePlugin => {}
        }
    }
    Ok(out)
}

fn append_registration(
    bytes: &[u8],
    spec: &HookRegistration,
    value: Value,
) -> Result<Vec<u8>, InstallError> {
    let (root_name, entry) = match spec.protocol {
        HookProtocol::CommandGroups { .. } => {
            let mut group = json!({"hooks":[value]});
            if let Some(matcher) = spec.matcher {
                group["matcher"] = matcher.into();
            }
            ("hooks".to_owned(), group)
        }
        HookProtocol::NamedGroups { .. } => {
            let entry = if let Some(matcher) = spec.matcher {
                json!({"matcher":matcher,"hooks":[value]})
            } else {
                value
            };
            (format!("provenant-{}", spec.action.command()), entry)
        }
        HookProtocol::CursorCommands { .. } => {
            let mut hook = value;
            if let Some(matcher) = spec.matcher {
                hook["matcher"] = matcher.into();
            }
            ("hooks".to_owned(), hook)
        }
        HookProtocol::OpenCodePlugin => return Ok(bytes.to_vec()),
    };
    let event_path = [root_name.as_str(), spec.event];
    match splice::get(bytes, &event_path)? {
        None => splice::set(bytes, &event_path, Some(&json!([entry]))),
        Some(Value::Array(entries)) => splice::set(
            bytes,
            &[root_name.as_str(), spec.event, &entries.len().to_string()],
            Some(&entry),
        ),
        Some(_) => Err(InstallError::InvalidConfig("hook event must be array")),
    }
}

/// Reconcile the exact managed leaf while preserving neighboring settings.
pub(crate) struct Registration {
    pub bytes: Vec<u8>,
    pub changes: Vec<SelectorChange>,
    pub owned: Vec<(String, Value)>,
    pub relinquished_modified: bool,
}
pub(crate) fn register(
    harness: Harness,
    bytes: &[u8],
    targets: &Targets,
    inherited: &BTreeSet<String>,
    follows_siblings: bool,
) -> Result<Registration, InstallError> {
    let mut out = bytes.to_vec();
    let mut changes = Vec::new();
    let mut owned = Vec::new();
    let mut relinquished_modified = false;
    for spec in targets.hook_specs.iter().filter(|spec| {
        spec.harness == harness && !matches!(spec.protocol, HookProtocol::OpenCodePlugin)
    }) {
        let desired = value(spec, targets)?;
        let command = desired
            .get("command")
            .and_then(Value::as_str)
            .ok_or(InstallError::InvalidConfig("hook command"))?;
        let sel = selector(spec, command)?;
        let was_owned = follows_siblings
            || inherited.iter().any(|selector| {
                read_identity(selector).is_ok_and(|old| {
                    old.harness == spec.harness.spec().name && old.action == spec.action.command()
                })
            });
        if !targets.hooks && !was_owned {
            continue;
        }
        if let Some(existing) = get(&out, &sel)? {
            if existing != desired {
                if !targets.hooks {
                    relinquished_modified = true;
                    continue;
                }
                return Err(InstallError::InvalidConfig(
                    "managed hook registration changed; rerun install with --hooks off to relinquish edited hooks",
                ));
            }
        } else {
            out = append_registration(&out, spec, desired.clone())?;
            changes.push(SelectorChange {
                selector: sel.clone(),
                before: None,
                after: Some(desired.clone()),
            });
        }
        owned.push((sel, desired));
    }
    Ok(Registration {
        bytes: out,
        changes,
        owned,
        relinquished_modified,
    })
}

fn render_settings(harness: Harness, binary: &Path, state: &Path) -> Result<Value, InstallError> {
    let mut settings = b"{}\n".to_vec();
    for spec in registrations_for(&[harness], true) {
        if matches!(spec.protocol, HookProtocol::OpenCodePlugin) {
            continue;
        }
        let value = hook_value(&spec, binary, state)?;
        settings = append_registration(&settings, &spec, value)?;
    }
    decode_jsonc(Cursor::new(settings), Limits::CONFIG).map_err(InstallError::from)
}

/// Native configuration document for a harness with file-backed hook settings.
pub fn lane_hook_settings(
    harness: Harness,
    binary: &Path,
    state: &Path,
) -> Result<Value, InstallError> {
    render_settings(harness, binary, state)
}

/// OpenCode discovers TypeScript plugins in the declared global plugin directory.
pub fn hook_plugin_source(binary: &Path, state: &Path) -> Result<Vec<u8>, InstallError> {
    let binary = binary
        .to_str()
        .ok_or(InstallError::InvalidConfig("hook executable must be UTF-8"))?;
    let state = state
        .to_str()
        .ok_or(InstallError::InvalidConfig("hook state root must be UTF-8"))?;
    let rows = registrations_for(&[Harness::Opencode], true)
        .into_iter()
        .map(|spec| {
            json!({
                "action": spec.action.command(),
                "event": spec.event,
                "matcher": spec.matcher,
                "timeout": spec.timeout_seconds,
            })
        })
        .collect::<Vec<_>>();
    let rows = serde_json::to_string(&rows)
        .map_err(|_| InstallError::InvalidConfig("hook plugin declaration"))?;
    let binary = serde_json::to_string(binary)
        .map_err(|_| InstallError::InvalidConfig("hook executable"))?;
    let state =
        serde_json::to_string(state).map_err(|_| InstallError::InvalidConfig("hook state root"))?;
    let adapter = serde_json::to_string(Harness::Opencode.spec().name)
        .map_err(|_| InstallError::InvalidConfig("hook adapter"))?;
    Ok(format!(
        r#"import {{ Plugin }} from "@opencode/plugin"

const binary = {binary}
const state = {state}
const adapter = {adapter}
const registrations = {rows}
const compacted = new Set<string>()
const started = new Set<string>()

function contextOf(value: any): string | undefined {{
  const context = value?.hookSpecificOutput?.additionalContext
  const text = typeof context === "string" ? context.trim() : ""
  return text.length > 0 ? text : undefined
}}

function addSystem(event: any, text: string | undefined) {{
  if (!text) return
  event.system ??= []
  event.system.push({{ type: "text", text }})
}}

async function invoke(action: string, eventName: string, payload: any, timeoutSeconds = 5): Promise<any | undefined> {{
  let child: ReturnType<typeof Bun.spawn> | undefined
  try {{
    child = Bun.spawn([binary, "--state-dir", state, "hook", adapter, action], {{
      stdin: "pipe", stdout: "pipe", stderr: "pipe",
    }})
    child.stdin.write(JSON.stringify({{ hook_event_name: eventName, ...payload }}))
    child.stdin.end()
    const stdout = new Response(child.stdout).text()
    const stderr = new Response(child.stderr).text()
    let timer: ReturnType<typeof setTimeout> | undefined
    const status = await Promise.race([
      child.exited,
      new Promise<number>((resolve) => {{
        timer = setTimeout(() => {{ child?.kill(); resolve(-1) }}, timeoutSeconds * 1000)
      }}),
    ])
    if (timer) clearTimeout(timer)
    const [text, diagnostic] = await Promise.all([stdout, stderr])
    const boundedDiagnostic = diagnostic.trim().slice(-1000)
    if (boundedDiagnostic) console.warn(`provenant hook ${{action}} diagnostic: ${{boundedDiagnostic}}`)
    if (status !== 0 || child.exitCode !== 0) {{
      console.warn(`provenant hook ${{action}} abstained (exit ${{status}})`)
      return undefined
    }}
    if (action === "pre-compact") return text.trim() || undefined
    try {{ return JSON.parse(text) }} catch (error) {{
      if (text.trim()) console.warn(`provenant hook ${{action}} returned invalid JSON: ${{String(error)}}`)
      return undefined
    }}
  }} catch (error) {{
    child?.kill()
    console.warn(`provenant hook ${{action}} abstained: ${{String(error)}}`)
    return undefined
  }}
}}

async function sessionPayload(ctx: any, event: any, source?: string): Promise<any | undefined> {{
  const sessionID = event?.sessionID ?? event?.properties?.sessionID
  if (typeof sessionID !== "string") {{
    console.warn("provenant OpenCode hook skipped: event has no session ID")
    return undefined
  }}
  try {{
    const session = await ctx.session.get({{ sessionID }})
    const cwd = session?.location?.directory
    if (session?.id !== sessionID || typeof cwd !== "string" || cwd.length === 0) {{
      console.warn(`provenant OpenCode hook skipped: session ${{sessionID}} has no declared location`)
      return undefined
    }}
    return {{ session_id: sessionID, cwd, ...(source ? {{ source }} : {{}}) }}
  }} catch (error) {{
    console.warn(`provenant OpenCode hook could not resolve session location: ${{String(error)}}`)
    return undefined
  }}
}}

export default Plugin.define({{
  id: "provenant",
  async setup(ctx) {{
    const byAction = new Map(registrations.map((row: any) => [row.action, row]))
    const sessionHooks: any[] = []
    const controllers: AbortController[] = []
    const preCompact = byAction.get("pre-compact") as any
    const compact = byAction.get("compact") as any
    const sessionStart = byAction.get("session-start") as any
    const stop = byAction.get("stop") as any

    if (preCompact) {{
      sessionHooks.push(await ctx.session.hook("compaction", async (event: any) => {{
        const payload = await sessionPayload(ctx, event)
        if (!payload) return
        const output = await invoke(preCompact.action, preCompact.event, payload, preCompact.timeout)
        addSystem(event, typeof output === "string" ? output : contextOf(output))
      }}))
    }}

    if (compact) {{
      const controller = new AbortController()
      controllers.push(controller)
      void (async () => {{
        try {{
          for await (const event of ctx.event.subscribe({{ types: [compact.event], signal: controller.signal }})) {{
            const sessionID = event?.properties?.sessionID ?? event?.sessionID
            if (typeof sessionID === "string") compacted.add(sessionID)
          }}
        }} catch (error) {{ console.warn(`provenant compaction event listener stopped: ${{String(error)}}`) }}
      }})()
    }}

    if (sessionStart) {{
      sessionHooks.push(await ctx.session.hook(sessionStart.event.slice("session.".length), async (event: any) => {{
        const sessionID = event?.sessionID
        const needsStart = typeof sessionID === "string" && !started.has(sessionID)
        const needsCompact = compact && typeof sessionID === "string" && compacted.has(sessionID)
        if (needsStart || needsCompact) {{
          const payload = await sessionPayload(ctx, event)
          if (!payload) return
          if (needsStart) {{
            const output = await invoke(sessionStart.action, sessionStart.event, payload, sessionStart.timeout)
            const context = contextOf(output)
            if (context) {{
              addSystem(event, context)
              started.add(sessionID)
            }}
          }}
          if (needsCompact) {{
            const compactPayload = {{ ...payload, source: "compact" }}
            const compactOutput = await invoke(compact.action, compact.event, compactPayload, compact.timeout)
            const compactContext = contextOf(compactOutput)
            if (compactContext) {{
              addSystem(event, compactContext)
              compacted.delete(sessionID)
            }}
          }}
        }}
      }}))
    }}

    if (stop) {{
      const controller = new AbortController()
      controllers.push(controller)
      void (async () => {{
        try {{
          for await (const event of ctx.event.subscribe({{ types: [stop.event], signal: controller.signal }})) {{
            const payload = await sessionPayload(ctx, event)
            if (!payload) continue
            await invoke(stop.action, stop.event, payload, stop.timeout)
          }}
        }} catch (error) {{ console.warn(`provenant stop event listener stopped: ${{String(error)}}`) }}
      }})()
    }}

    return async () => {{
      for (const controller of controllers) controller.abort()
      await Promise.all(sessionHooks.map((registration: any) => registration.dispose()))
    }}
  }},
}})
"#
    )
    .into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::{ADAPTER_SPECS, HookAction};

    fn declared(
        harness: Harness,
        lane: bool,
    ) -> Vec<(HookAction, &'static str, Option<&'static str>, Option<u64>)> {
        let spec = harness.spec();
        let Some(install) = spec.install else {
            return Vec::new();
        };
        install
            .hook_events
            .iter()
            .filter_map(|event| {
                let HookEvent::Supported {
                    action,
                    event,
                    matcher,
                    timeout_seconds,
                } = event
                else {
                    return None;
                };
                if (!spec.chair_switch
                    && matches!(
                        action,
                        HookAction::ChairPrompt | HookAction::ChairStart | HookAction::ChairStop
                    ))
                    || (*action == HookAction::PreToolUse
                        && (lane || !install.answer_delivery.is_supported()))
                {
                    return None;
                }
                Some((*action, *event, *matcher, *timeout_seconds))
            })
            .collect()
    }

    #[test]
    fn renderers_project_declared_native_and_lane_hook_actions() {
        let binary = Path::new("/private/bin/provenant");
        let state = Path::new("/private/state");
        for adapter in ADAPTER_SPECS {
            let Some(install) = adapter.install else {
                continue;
            };
            if !install.hooks.is_supported() {
                continue;
            }
            let native = registrations(&[adapter.id]);
            let declared_native = declared(adapter.id, false);
            assert_eq!(
                native.len(),
                declared_native.len(),
                "{} native",
                adapter.name
            );
            for (action, event, matcher, timeout) in declared_native {
                assert_eq!(
                    native
                        .iter()
                        .filter(|row| {
                            row.action == action
                                && row.event == event
                                && row.matcher == matcher
                                && row.timeout_seconds == timeout
                        })
                        .count(),
                    1,
                    "{} {:?}",
                    adapter.name,
                    action
                );
            }

            let lane = registrations_for(&[adapter.id], true);
            let declared_lane = declared(adapter.id, true);
            assert_eq!(lane.len(), declared_lane.len(), "{} lane", adapter.name);
            for (action, event, matcher, timeout) in declared_lane {
                assert_eq!(
                    lane.iter()
                        .filter(|row| {
                            row.action == action
                                && row.event == event
                                && row.matcher == matcher
                                && row.timeout_seconds == timeout
                        })
                        .count(),
                    1,
                    "{} lane {:?}",
                    adapter.name,
                    action
                );
            }

            if matches!(install.hook_protocol, Some(HookProtocol::OpenCodePlugin)) {
                let source = String::from_utf8(hook_plugin_source(binary, state).unwrap()).unwrap();
                for row in &lane {
                    let encoded = serde_json::to_string(&json!({
                        "action": row.action.command(),
                        "event": row.event,
                        "matcher": row.matcher,
                        "timeout": row.timeout_seconds,
                    }))
                    .unwrap();
                    assert!(
                        source.contains(&encoded),
                        "{} {:?}",
                        adapter.name,
                        row.action
                    );
                }
                assert!(!source.contains("\"action\":\"pre-tool-use\""));
            } else {
                let settings = lane_hook_settings(adapter.id, binary, state).unwrap();
                let bytes = serde_json::to_vec(&settings).unwrap();
                for row in &lane {
                    let value = hook_value(row, binary, state).unwrap();
                    let command = value.get("command").and_then(Value::as_str).unwrap();
                    let selector = selector(row, command).unwrap();
                    assert_eq!(get(&bytes, &selector).unwrap(), Some(value));
                }
            }
        }
    }
}
