//! Small command edge for the checked-in replay bench script.
use crate::{emit, fail, roots};
use provenant_cli::{Bench, Globals};
use provenant_records::{CommandResult, Outcome};
use provenant_supervisor::exec::{
    ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
};
use serde_json::Value;
use std::{ffi::OsString, time::Duration};

pub fn run(command: Bench, globals: &Globals) -> u8 {
    let leaf = match &command {
        Bench::Mine(_) => "bench mine",
        Bench::Verify { .. } => "bench verify",
        Bench::Run(_) => "bench run",
    };
    let result = (|| -> Result<Value, Box<dyn std::error::Error>> {
        let project = roots::selected_project(globals.project.as_deref(), true)?;
        let script = project.join("scripts/bench");
        if !script.is_file() {
            return Err("bench script is missing from the selected repository".into());
        }
        let mut argv = vec![OsString::from("/bin/sh"), script.into_os_string()];
        match command {
            Bench::Mine(mine) => {
                argv.push("mine".into());
                if let Some(limit) = mine.limit {
                    argv.extend(["--limit".into(), limit.to_string().into()]);
                }
            }
            Bench::Verify { task } => {
                argv.extend(["verify".into(), task.into_os_string()]);
            }
            Bench::Run(run) => {
                argv.extend(["run".into(), "--route".into(), run.route.into()]);
                let split = match provenant_cli::bench::selected_split(Some(run.split)) {
                    provenant_cli::bench::Split::Search => "search",
                    provenant_cli::bench::Split::Acceptance => "acceptance",
                    provenant_cli::bench::Split::HeldOut => "held-out",
                };
                argv.extend(["--split".into(), split.into()]);
                if let Some(limit) = run.limit {
                    argv.extend(["--limit".into(), limit.to_string().into()]);
                }
            }
        }
        let binary = std::env::current_exe()?;
        let mut environment: Vec<_> = std::env::vars_os().collect();
        environment.retain(|(name, _)| name != "PROVENANT_BIN");
        environment.push(("PROVENANT_BIN".into(), binary.into_os_string()));
        let mut options = ExecOptions::new(&argv, &project, Duration::from_secs(86_400));
        options.env = Some(&environment);
        options.stdout = StreamPolicy::Capture { limit: 1024 * 1024 };
        options.stderr = StreamPolicy::Capture { limit: 64 * 1024 };
        options.stop_on_output_limit = true;
        let output = SystemExecutor::new(1)?.execute(options)?;
        if output.termination != Termination::Exit(0)
            || output.timed_out
            || output.stdout.truncated
            || output.stdout.read_failed
        {
            let stderr = String::from_utf8_lossy(&output.stderr.bytes);
            return Err(format!("{leaf} failed: {}", stderr.trim()).into());
        }
        let text = String::from_utf8_lossy(&output.stdout.bytes)
            .trim()
            .to_owned();
        Ok(serde_json::json!({ "output": text }))
    })();
    match result {
        Ok(data) => {
            let human = data["output"].as_str().unwrap_or_default().to_owned() + "\n";
            emit(
                &CommandResult::new(leaf, Outcome::Ok, data),
                globals,
                &human,
                false,
            )
        }
        Err(error) => fail(leaf, Outcome::Error, "BENCH", &error.to_string(), globals),
    }
}
