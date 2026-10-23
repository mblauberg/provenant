use super::*;

pub fn agy(plan: &Plan) -> Result<AdapterProfile, crate::pending::IntegerError> {
    let mut argv = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--disable-slash-commands".into(),
        "--print-timeout".into(),
        format!(
            "{}s",
            crate::pending::positive_timeout(&plan.timeout_seconds)?
        ),
    ];
    if plan.mode.is_writer() {
        argv.push("--dangerously-skip-permissions".into());
    } else if plan.agy_sandbox {
        argv.push("--sandbox".into());
    }
    if let Some(session) = plan
        .resume_session
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        argv.extend(["--conversation".into(), session.clone()]);
    }
    if let Some(model) = plan.model.as_ref().filter(|value| !value.is_empty()) {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = plan.effort.as_ref().filter(|value| !value.is_empty()) {
        argv.extend(["--effort".into(), effort.clone()]);
    }
    for directory in &plan.add_dirs {
        argv.extend(["--add-dir".into(), directory.clone()]);
    }
    argv.extend([
        "--print".into(),
        format!("{}\nTask:\n{}", plan.boundary_prompt, plan.prompt),
    ]);
    Ok(profile("agy", argv))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn plan(mode: Mode) -> Plan {
        Plan {
            mode,
            prompt: "do it".into(),
            boundary_prompt: "Read only".into(),
            timeout_seconds: serde_json::json!(0),
            agy_sandbox: true,
            resume_session: None,
            model: Some("model-x".into()),
            effort: Some("high".into()),
            session_id: "session-1".into(),
            cwd: "/repo".into(),
            add_dirs: vec!["/shared".into()],
            endpoint_base_url: None,
            context_ceiling: Some(serde_json::json!(1000)),
        }
    }

    #[test]
    fn agy_clamps_timeout_and_selects_mode_permission() {
        let read = agy(&plan(Mode::ReadOnly)).unwrap();
        assert_eq!(
            &read.argv[..7],
            &[
                "agy",
                "--output-format",
                "stream-json",
                "--disable-slash-commands",
                "--print-timeout",
                "1s",
                "--sandbox"
            ]
        );
        let writer = agy(&plan(Mode::WorktreeWrite)).unwrap();
        assert!(writer
            .argv
            .contains(&"--dangerously-skip-permissions".into()));
        assert_eq!(writer.argv.last().unwrap(), "Read only\nTask:\ndo it");
    }
}

#[cfg(test)]
mod coercion_tests {
    use super::*;
    #[test]
    fn timeout_coercion_matches_python_including_failures() {
        let values = serde_json::json!([null, false, true, -3, -0.1, 0, 0.9, 1.9, 30, " -2 ", "1_000", "١٢", "１２", "\u{1c}+4\u{1f}", "", "1__2", "_1", "1_", "1.2", [], {}]);
        let actual: Vec<_> = values.as_array().unwrap().iter().map(|value| {
            let mut p = super::tests::plan(Mode::ReadOnly);
            p.timeout_seconds = value.clone();
            agy(&p).map(|profile| serde_json::json!(profile.argv)).unwrap_or(serde_json::Value::Null)
        }).collect();
        let Some(expected): Option<Vec<serde_json::Value>> = crate::oracle::python(r#"
import json,sys
from skills.orchestrate.scripts.adapters.agy import argv
out=[]
for v in json.load(sys.stdin):
    p=dict(timeout_seconds=v,mode='read_only',agy_sandbox=True,resume_session=None,model='model-x',effort='high',applied={'add_dirs':['/shared']},boundary_prompt='Read only',prompt='do it')
    try: out.append(argv(p))
    except (ValueError,TypeError,OverflowError): out.append(None)
print(json.dumps(out))
"#, &values, None) else { return; };
        assert_eq!(actual, expected);
        assert_eq!(crate::pending::positive_timeout(&serde_json::json!("18446744073709551616")), Err(crate::IntegerError::NumberOutOfRange));
    }
}
