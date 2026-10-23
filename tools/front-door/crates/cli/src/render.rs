//! Encode completely before delivery; never clip a result or repeat an owner call.
use provenant_records::{CommandResult, Outcome};
use serde::Serialize;
use std::io::{self, Write};
use thiserror::Error;
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;
pub const MAX_INPUT_BYTES: u64 = 1024 * 1024;
#[derive(Clone, Copy)]
pub enum RenderMode<'a> {
    Json,
    Human { text: &'a str, read_only: bool },
}
#[derive(Debug, Error)]
pub enum RenderError {
    #[error("result exceeds 1 MiB; read the owner-published artifact or request a smaller page")]
    TooLarge,
    #[error("cannot encode command result")]
    Encode(#[source] serde_json::Error),
    #[error("cannot write command result")]
    Write(#[source] io::Error),
}
pub fn exit_status(outcome: &Outcome) -> u8 {
    match outcome {
        Outcome::Ok => 0,
        Outcome::Failed | Outcome::Refused => 1,
        Outcome::Invalid => 2,
        Outcome::Error => 3,
        Outcome::Unavailable => 4,
        Outcome::Timeout => 124,
        Outcome::Cancelled => 130,
    }
}
/// At most cap bytes are retained even if an owner's Serialize floods output.
struct Buffer {
    bytes: Vec<u8>,
    overflow: bool,
}
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_RESULT_BYTES.saturating_sub(self.bytes.len()) {
            self.overflow = true;
            return Err(io::Error::other("result limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn render<T: Serialize>(
    result: &CommandResult<T>,
    mode: RenderMode<'_>,
    writer: &mut impl Write,
) -> Result<(), RenderError> {
    let mut buffer = Buffer {
        bytes: Vec::new(),
        overflow: false,
    };
    let encoded = match mode {
        RenderMode::Json => serde_json::to_writer(&mut buffer, result)
            .map_err(RenderError::Encode)
            .and_then(|()| buffer.write_all(b"\n").map_err(RenderError::Write)),
        RenderMode::Human { text, .. } => {
            if let Some(error) = &result.error {
                writeln!(buffer, "provenant: {}: {}", error.code, error.message)
                    .map_err(RenderError::Write)
            } else {
                buffer
                    .write_all(text.as_bytes())
                    .map_err(RenderError::Write)
            }
        }
    };
    if buffer.overflow {
        return Err(RenderError::TooLarge);
    }
    encoded?;
    match writer
        .write_all(&buffer.bytes)
        .and_then(|()| writer.flush())
    {
        Ok(()) => Ok(()),
        Err(error)
            if error.kind() == io::ErrorKind::BrokenPipe
                && matches!(
                    mode,
                    RenderMode::Human {
                        read_only: true,
                        ..
                    }
                )
                && result.status == Outcome::Ok =>
        {
            Ok(())
        }
        Err(error) => Err(RenderError::Write(error)),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversize_is_rejected_before_any_stdout_and_lf_counts() {
        let result = CommandResult::new("output", Outcome::Ok, "x".repeat(MAX_RESULT_BYTES));
        let mut out = Vec::new();
        assert!(matches!(
            render(&result, RenderMode::Json, &mut out),
            Err(RenderError::TooLarge)
        ));
        assert!(out.is_empty());
        let text = "x".repeat(MAX_RESULT_BYTES);
        assert!(
            render(
                &result,
                RenderMode::Human {
                    text: &text,
                    read_only: true
                },
                &mut out
            )
            .is_ok()
        );
        assert_eq!(out.len(), MAX_RESULT_BYTES);
        out.clear();
        assert!(matches!(
            render(
                &result,
                RenderMode::Human {
                    text: &format!("{text}\n"),
                    read_only: true
                },
                &mut out
            ),
            Err(RenderError::TooLarge)
        ));
        assert!(out.is_empty());
    }
    #[test]
    fn all_outcomes_and_partial_input_required_data_keep_their_contract()
    -> Result<(), Box<dyn std::error::Error>> {
        for (outcome, code) in [
            (Outcome::Ok, 0),
            (Outcome::Failed, 1),
            (Outcome::Refused, 1),
            (Outcome::Invalid, 2),
            (Outcome::Error, 3),
            (Outcome::Unavailable, 4),
            (Outcome::Timeout, 124),
            (Outcome::Cancelled, 130),
        ] {
            assert_eq!(exit_status(&outcome), code);
        }
        #[derive(Serialize)]
        struct Data {
            partial: bool,
            state: &'static str,
        }
        let result = CommandResult::new(
            "lanes",
            Outcome::Ok,
            Data {
                partial: true,
                state: "input_required",
            },
        );
        let mut bytes = Vec::new();
        render(&result, RenderMode::Json, &mut bytes)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(value["data"]["partial"], true);
        assert_eq!(value["status"], "ok");
        Ok(())
    }
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn broken_pipe_only_succeeds_for_human_read_only_results() {
        let result = CommandResult::new("lanes", Outcome::Ok, ());
        assert!(
            render(
                &result,
                RenderMode::Human {
                    text: "lanes",
                    read_only: true
                },
                &mut Broken
            )
            .is_ok()
        );
        assert!(matches!(
            render(&result, RenderMode::Json, &mut Broken),
            Err(RenderError::Write(_))
        ));
        assert!(matches!(
            render(
                &result,
                RenderMode::Human {
                    text: "accepted",
                    read_only: false
                },
                &mut Broken
            ),
            Err(RenderError::Write(_))
        ));
    }
}

/// Overflow publication is an I/O seam supplied by the composition root. A
/// committed result is passed once; this function never calls its owner again.
pub fn render_or_publish<T: Serialize>(
    result: &CommandResult<T>,
    mode: RenderMode<'_>,
    writer: &mut impl Write,
    stderr: &mut impl Write,
    publish: impl FnOnce(&CommandResult<T>) -> io::Result<provenant_records::AbsolutePath>,
) -> Result<Outcome, RenderError> {
    let first = if matches!(mode, RenderMode::Human { .. }) && result.error.is_some() {
        render(result, mode, stderr)
    } else {
        render(result, mode, writer)
    };
    match first {
        Ok(()) => Ok(result.status),
        Err(RenderError::TooLarge) => {
            let (code, message, artifact) = match publish(result) {
                Ok(path) => (
                    "RESULT_TOO_LARGE",
                    "result exceeds 1 MiB; read the published artifact",
                    Some(path),
                ),
                Err(_) => (
                    "IO",
                    "cannot publish oversized result; the recorded operation must not be repeated",
                    None,
                ),
            };
            #[derive(Serialize)]
            struct Overflow {
                artifact: Option<provenant_records::AbsolutePath>,
            }
            let mut failure =
                CommandResult::new(&result.command, Outcome::Error, Overflow { artifact });
            failure.error = Some(provenant_records::CommandError {
                code: code.into(),
                message: message.into(),
                cause: None,
                repair: None,
            });
            // Human transport includes the pointer using the same typed path
            // record, escaped as JSON. JSON carries it in data.artifact.
            let human = match &failure.data.artifact {
                Some(path) => format!(
                    "provenant: {code}: {message}; {}\n",
                    serde_json::to_string(path).map_err(RenderError::Encode)?
                ),
                None => format!("provenant: {code}: {message}\n"),
            };
            let fallback_mode = match mode {
                RenderMode::Json => RenderMode::Json,
                RenderMode::Human { .. } => RenderMode::Human {
                    text: &human,
                    read_only: false,
                },
            };
            if matches!(fallback_mode, RenderMode::Json) {
                render(&failure, fallback_mode, writer)?;
            } else {
                let text_result = CommandResult::new(&result.command, Outcome::Error, ());
                render(&text_result, fallback_mode, stderr)?;
            }
            Ok(Outcome::Error)
        }
        Err(error) => Err(error),
    }
}
#[cfg(test)]
mod overflow_tests {
    use super::*;
    #[test]
    fn oversize_publishes_once_or_reports_io_without_partial_output()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = CommandResult::new("dispatch", Outcome::Ok, "x".repeat(MAX_RESULT_BYTES));
        let mut output = Vec::new();
        let mut published = 0;
        let status = render_or_publish(
            &result,
            RenderMode::Json,
            &mut output,
            &mut Vec::new(),
            |_| {
                published += 1;
                provenant_records::AbsolutePath::new(std::path::Path::new("/fixture/result.json"))
                    .map_err(io::Error::other)
            },
        )?;
        assert_eq!(status, Outcome::Error);
        assert_eq!(published, 1);
        let value: serde_json::Value = serde_json::from_slice(&output)?;
        assert_eq!(value["error"]["code"], "RESULT_TOO_LARGE");
        assert!(value["data"]["artifact"].is_object());
        output.clear();
        render_or_publish(
            &result,
            RenderMode::Json,
            &mut output,
            &mut Vec::new(),
            |_| Err(io::ErrorKind::StorageFull.into()),
        )?;
        let value: serde_json::Value = serde_json::from_slice(&output)?;
        assert_eq!(value["error"]["code"], "IO");
        assert!(value["data"]["artifact"].is_null());
        Ok(())
    }
}
