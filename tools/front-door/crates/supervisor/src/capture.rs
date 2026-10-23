//! Bounded head/tail views over file-backed output; never read the whole spool.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Seek, SeekFrom};
use thiserror::Error;

const MARKER: &[u8] = b"\n...[output truncated]...\n";

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("could not read bounded output")]
    Read(#[source] io::Error),
    #[error("process text is not UTF-8")]
    Decode(#[source] std::string::FromUtf8Error),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoundedProcessResult {
    pub returncode: i32,
    pub output: String,
    pub output_bytes: u64,
    pub output_truncated: bool,
    pub timed_out: bool,
    pub elapsed_seconds: f64,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub stdout_bytes: Option<u64>,
    pub stderr_bytes: Option<u64>,
    pub stdout_truncated: Option<bool>,
    pub stderr_truncated: Option<bool>,
}

impl BoundedProcessResult {
    pub fn terminating_signal(&self) -> Option<u32> {
        (self.returncode < 0).then(|| self.returncode.unsigned_abs())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedOutput {
    pub text: String,
    pub bytes: u64,
    pub truncated: bool,
}

pub fn read_bounded_output(
    spool: &mut (impl Read + Seek),
    limit: i64,
) -> Result<CapturedOutput, CaptureError> {
    let (retained, bytes, truncated) =
        read_bytes(spool, limit.max(0) as u64).map_err(CaptureError::Read)?;
    Ok(CapturedOutput {
        text: String::from_utf8_lossy(&retained).into_owned(),
        bytes,
        truncated,
    })
}

/// subprocess.run(text=True): strict UTF-8 plus universal newline translation.
#[cfg(test)]
pub(crate) fn read_bounded_text(
    spool: &mut (impl Read + Seek),
    limit: i64,
) -> Result<CapturedOutput, CaptureError> {
    let (retained, bytes, truncated) =
        read_bytes(spool, limit.max(0) as u64).map_err(CaptureError::Read)?;
    let text = String::from_utf8(retained).map_err(CaptureError::Decode)?;
    Ok(CapturedOutput {
        text: text.replace("\r\n", "\n").replace('\r', "\n"),
        bytes,
        truncated,
    })
}

fn read_bytes(spool: &mut (impl Read + Seek), limit: u64) -> io::Result<(Vec<u8>, u64, bool)> {
    let bytes = spool.seek(SeekFrom::End(0))?;
    let truncated = bytes > limit;
    let mut retained = Vec::new();
    spool.rewind()?;
    if !truncated {
        spool.take(bytes).read_to_end(&mut retained)?;
    } else {
        let marker = if limit > MARKER.len() as u64 {
            MARKER
        } else {
            &[]
        };
        let available = limit - marker.len() as u64;
        let head = available / 2;
        let tail = available - head;
        spool.take(head).read_to_end(&mut retained)?;
        retained.extend_from_slice(marker);
        if tail != 0 {
            // tail <= i64::MAX: the public limit has that bound.
            spool.seek(SeekFrom::End(-(tail as i64)))?;
            spool.take(tail).read_to_end(&mut retained)?;
        }
    }
    Ok((retained, bytes, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn head_and_tail_survive_large_output() {
        let mut bytes = b"HEAD-".to_vec();
        bytes.extend(vec![b'x'; 4096]);
        bytes.extend(b"-TAIL");
        let view = read_bounded_output(&mut Cursor::new(bytes), 128).unwrap();
        assert!(view.text.starts_with("HEAD-"));
        assert!(view.text.ends_with("-TAIL"));
        assert_eq!(view.text.len(), 128);
        assert_eq!(view.bytes, 4106);
        assert!(view.truncated);
    }

    #[test]
    fn zero_negative_and_small_limits_preserve_python_policy() {
        for limit in [-1, 0, 1, 2, 24, 25] {
            let view = read_bounded_output(&mut Cursor::new(vec![b'x'; 50]), limit).unwrap();
            assert_eq!(view.text.len(), limit.max(0) as usize);
            assert!(view.truncated);
        }
        let view = read_bounded_output(&mut Cursor::new(b"ok"), 2).unwrap();
        assert!(!view.truncated);
        assert_eq!(view.text, "ok");
    }

    #[test]
    fn utf8_replacement_occurs_after_the_byte_limit() {
        let view = read_bounded_output(&mut Cursor::new("λλ".as_bytes()), 2).unwrap();
        assert_eq!(view.text, "λ");
        let one = read_bounded_output(&mut Cursor::new("λλ".as_bytes()), 1).unwrap();
        assert_eq!(one.text, "�");
        assert_eq!(view.bytes, 4);
    }

    #[test]
    fn broken_spool_is_a_typed_failure() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("lost spool"))
            }
        }
        impl Seek for Broken {
            fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
                Err(io::Error::other("lost spool"))
            }
        }
        assert!(matches!(
            read_bounded_output(&mut Broken, 1),
            Err(CaptureError::Read(_))
        ));
    }

    #[test]
    fn result_record_keeps_null_streams_and_negative_signal_status() {
        let value = BoundedProcessResult {
            returncode: -15,
            output: "OUTERR".into(),
            output_bytes: 6,
            output_truncated: false,
            timed_out: false,
            elapsed_seconds: 1.0,
            stdout: None,
            stderr: None,
            stdout_bytes: None,
            stderr_bytes: None,
            stdout_truncated: None,
            stderr_truncated: None,
        };
        assert_eq!(value.terminating_signal(), Some(15));
        let json = serde_json::to_value(value).unwrap();
        assert!(json.get("stdout").unwrap().is_null());
        assert_eq!(json["returncode"], -15);
    }
}

#[cfg(test)]
mod text_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn subprocess_text_is_strict_and_translates_newlines_while_bounded_capture_replaces() {
        let bytes = b"head\r\ntwo\rtail\n";
        assert_eq!(
            read_bounded_text(&mut Cursor::new(bytes), 128)
                .unwrap()
                .text,
            "head\ntwo\ntail\n"
        );
        assert_eq!(
            read_bounded_output(&mut Cursor::new(bytes), 128)
                .unwrap()
                .text,
            "head\r\ntwo\rtail\n"
        );
        assert!(matches!(
            read_bounded_text(&mut Cursor::new(b"bad\xff"), 128),
            Err(CaptureError::Decode(_))
        ));
        assert_eq!(
            read_bounded_output(&mut Cursor::new(b"bad\xff"), 128)
                .unwrap()
                .text,
            "bad�"
        );
    }
}
