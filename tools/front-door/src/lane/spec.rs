use crate::json::{self, Value};
use std::collections::BTreeMap;
use std::path::{Component, PathBuf};

#[derive(Debug)]
pub struct Spec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub input: Option<PathBuf>,
    pub attempt: PathBuf,
    pub token: String,
    pub events: PathBuf,
    pub stderr: PathBuf,
    pub cap: usize,
    pub silence: u64,
    pub startup: u64,
    pub wall: u64,
    pub settle: u64,
    pub child_grace: u64,
    pub root_flush: u64,
    pub writer: Option<(PathBuf, Vec<String>)>,
    pub markers: Vec<PathBuf>,
    pub observer: Vec<String>,
    pub spare_host: bool,
}
fn required<'a>(v: &'a Value, k: &str) -> Result<&'a Value, String> {
    v.get(k).ok_or_else(|| format!("missing {k}"))
}
pub fn string(v: &Value) -> Result<String, String> {
    match v {
        Value::Str(s) if !s.contains('\0') => Ok(s.clone()),
        _ => Err("expected string without NUL".into()),
    }
}
fn strings(v: &Value) -> Result<Vec<String>, String> {
    match v {
        Value::Array(a) => a.iter().map(string).collect(),
        _ => Err("expected string array".into()),
    }
}
fn number(v: &Value, k: &str) -> Result<u64, String> {
    match required(v, k)? {
        Value::Int(n) if *n >= 0 => Ok(*n as u64),
        _ => Err(format!(
            "{k} must be nonnegative integer milliseconds/bytes"
        )),
    }
}
fn absolute(v: &Value) -> Result<PathBuf, String> {
    let p = PathBuf::from(string(v)?);
    if p.is_absolute() {
        Ok(p)
    } else {
        Err("expected absolute path".into())
    }
}
fn capture_path(v: &Value, k: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(string(required(v, k)?)?);
    if p.as_os_str().is_empty() || p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("{k} must be contained in attempt_dir"));
    }
    Ok(p)
}
impl Spec {
    pub fn parse(text: &str) -> Result<Self, String> {
        let v = json::parse(text)?;
        if string(required(&v, "schema")?)? != "provenant.lane.launch.v1" {
            return Err("unsupported launch schema; fix: scripts/install-harness".into());
        }
        let argv = strings(required(&v, "argv")?)?;
        if argv.is_empty() || argv[0].is_empty() {
            return Err("argv is empty".into());
        }
        let env = match required(&v, "env")? {
            Value::Object(pairs) => pairs
                .iter()
                .map(|(k, v)| {
                    if k.is_empty() || k.contains(['=', '\0']) {
                        return Err("invalid environment name".into());
                    }
                    Ok((k.clone(), string(v)?))
                })
                .collect::<Result<BTreeMap<_, _>, String>>()?,
            _ => return Err("env must be an object".into()),
        };
        let capture = required(&v, "capture")?;
        let timeouts = required(&v, "timeouts_ms")?;
        let input = match required(&v, "stdin")?.get("path") {
            None | Some(Value::Null) => None,
            Some(p) => Some(absolute(p)?),
        };
        let writer = match v.get("progress").and_then(|p| p.get("writer_tree")) {
            None | Some(Value::Null) => None,
            Some(w) => Some((
                absolute(required(w, "root")?)?,
                strings(required(w, "exclude")?)?,
            )),
        };
        let markers = strings(required(&v, "cancel_markers")?)?
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let observer = strings(required(required(&v, "observer")?, "argv")?)?;
        let spare_host = match required(required(&v, "spare")?, "code_mode_host")? {
            Value::Bool(b) => *b,
            _ => return Err("code_mode_host must be boolean".into()),
        };
        let cap =
            usize::try_from(number(capture, "max_bytes")?).map_err(|_| "capture cap overflows")?;
        if cap == 0 || cap > 64 * 1024 * 1024 {
            return Err("capture cap must be 1..67108864".into());
        }
        let silence = number(&v, "silence_observation_ms")?;
        if silence == 0 {
            return Err("silence observation must be positive".into());
        }
        Ok(Self {
            argv,
            cwd: absolute(required(&v, "cwd")?)?,
            env,
            input,
            attempt: absolute(required(&v, "attempt_dir")?)?,
            token: string(required(&v, "run_token")?)?,
            events: capture_path(capture, "events")?,
            stderr: capture_path(capture, "stderr")?,
            cap,
            silence,
            startup: number(timeouts, "startup")?,
            wall: number(timeouts, "wall")?,
            settle: number(timeouts, "settle")?,
            child_grace: number(timeouts, "child_grace")?,
            root_flush: number(timeouts, "root_flush")?,
            writer,
            markers,
            observer,
            spare_host,
        })
    }
    pub fn cancelled(&self) -> bool {
        self.markers.iter().any(|p| self.attempt.join(p).exists())
    }
    /// Redact only environment values; never re-serialise the Python spec.
    pub fn redacted(&self, source: &str) -> Result<String, String> {
        use super::files::quote;
        let env = format!(
            "{{{}}}",
            self.env
                .keys()
                .map(|s| format!("{}:{}", quote(s), quote(s)))
                .collect::<Vec<_>>()
                .join(",")
        );
        json::redact_member(source, "env", &env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn integer_fields_reject_fraction_exponent_and_overflow() {
        for text in ["1.5", "1e0", "999999999999999999999", "-1", "true"] {
            let v = json::parse(&format!("{{\"ms\":{text}}}")).unwrap();
            assert!(number(&v, "ms").is_err());
        }
    }
    #[test]
    fn unknown_major_rejected_before_fields() {
        assert!(
            Spec::parse(r#"{"schema":"provenant.lane.launch.v2"}"#)
                .unwrap_err()
                .contains("unsupported")
        );
    }
    #[test]
    fn capture_paths_cannot_escape() {
        for path in ["../out", "/tmp/out", "a/../../out", ""] {
            let v = json::parse(&format!(
                "{{\"events\":{}}}",
                super::super::files::quote(path)
            ))
            .unwrap();
            assert!(capture_path(&v, "events").is_err());
        }
    }
}
