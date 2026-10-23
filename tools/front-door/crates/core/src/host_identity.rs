//! Partial port: RP-1001 records the remaining Python compatibility work.
//! One host identity for Fabric, telemetry, quota and cooldowns.
use crate::json_support::unique_json;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const NAME_LIMIT: usize = 63;
pub const FALLBACK: &str = "local";

#[derive(Debug, Error)]
pub enum InvalidHost {
    #[error("Cannot read host configuration")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Host configuration must be strict JSON")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("Host configuration must be strict JSON")]
    Encoding {
        path: PathBuf,
        #[source]
        source: HostEncodingError,
    },
    #[error("local_host must be 1-63 lowercase letters, digits and hyphens")]
    Name,
}
/// Byte decoding failures retain their cause without exposing configuration data.
#[derive(Debug, Error)]
pub enum HostEncodingError {
    #[error("invalid UTF-8")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("invalid UTF-16")]
    Utf16(#[from] std::string::FromUtf16Error),
    #[error("invalid UTF-32 code point")]
    Utf32(#[from] std::char::CharTryFromError),
    #[error("truncated UTF-{bits} code unit")]
    Truncated { bits: u8 },
}

// Python json.loads(bytes) sniffs UTF-8/16/32 even without a BOM.
fn decode_bytes(raw: &[u8]) -> Result<String, HostEncodingError> {
    let (raw, width, little) = if let Some(s) = raw.strip_prefix(&[0, 0, 0xfe, 0xff]) {
        (s, 4, false)
    } else if let Some(s) = raw.strip_prefix(&[0xff, 0xfe, 0, 0]) {
        (s, 4, true)
    } else if let Some(s) = raw.strip_prefix(&[0xfe, 0xff]) {
        (s, 2, false)
    } else if let Some(s) = raw.strip_prefix(&[0xff, 0xfe]) {
        (s, 2, true)
    } else if let Some(s) = raw.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        (s, 1, false)
    } else if raw.len() >= 4 && raw[0] == 0 {
        (raw, if raw[1] == 0 { 4 } else { 2 }, false)
    } else if raw.len() >= 4 && raw[1] == 0 {
        (raw, if raw[2] != 0 || raw[3] != 0 { 2 } else { 4 }, true)
    } else if raw.len() == 2 && raw[0] == 0 {
        (raw, 2, false)
    } else if raw.len() == 2 && raw[1] == 0 {
        (raw, 2, true)
    } else {
        (raw, 1, false)
    };
    if width == 1 {
        return Ok(String::from_utf8(raw.to_vec())?);
    }
    if raw.len() % width != 0 {
        return Err(HostEncodingError::Truncated {
            bits: (width * 8) as u8,
        });
    }
    if width == 2 {
        let units: Vec<_> = raw
            .chunks_exact(2)
            .map(|b| {
                if little {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            })
            .collect();
        return Ok(String::from_utf16(&units)?);
    }
    let mut text = String::new();
    for b in raw.chunks_exact(4) {
        let value = if little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        };
        text.push(char::try_from(value)?);
    }
    Ok(text)
}

impl InvalidHost {
    pub fn code(&self) -> &'static str {
        "invalid_config"
    }
}

/// The environment-selected instance root, or `~/.agents` when absent/empty.
pub fn config_path(
    instance_root: Option<&Path>,
    homes: &crate::paths::Homes,
) -> Result<PathBuf, crate::paths::PathError> {
    let root = instance_root
        .filter(|v| !v.as_os_str().is_empty())
        .unwrap_or(Path::new("~/.agents"));
    Ok(crate::paths::expand_user(root, homes)?.join(".agent-fabric/hosts.json"))
}

pub fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= NAME_LIMIT
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

pub fn slug(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let mut text = String::new();
    for c in lower.split('.').next().unwrap_or_default().chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            c
        } else {
            '-'
        };
        if c != '-' || !text.ends_with('-') {
            text.push(c);
        }
    }
    let text = text.trim_matches('-');
    let text = &text[..text.len().min(NAME_LIMIT)]; // Slugs contain only ASCII.
    let text = text.trim_matches('-');
    if text.is_empty() {
        FALLBACK.into()
    } else {
        text.into()
    }
}

/// Python `slug(value)`: falsy inputs become an empty string before `str`.
/// JSON object iteration follows the input Value's order; callers needing
/// insertion order must retain it before constructing a serde Value.
pub fn slug_value(value: &Value) -> String {
    if crate::py::python_truth(value) {
        slug(&crate::py::python_str(value))
    } else {
        slug("")
    }
}

pub fn configured(path: &Path) -> Result<Option<String>, InvalidHost> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(InvalidHost::Read {
                path: path.into(),
                source,
            });
        }
    };
    let text = decode_bytes(&raw).map_err(|source| InvalidHost::Encoding {
        path: path.into(),
        source,
    })?;
    let value = unique_json(&text).map_err(|source| InvalidHost::Json {
        path: path.into(),
        source,
    })?;
    let local = value
        .get("local_host")
        .and_then(Value::as_str)
        .filter(|v| valid_name(v))
        .ok_or(InvalidHost::Name)?;
    Ok(Some(local.into()))
}

/// `hostname` is the observed socket hostname, or an empty string on probe failure.
pub fn resolve(path: &Path, hostname: &str) -> (String, Option<InvalidHost>) {
    match configured(path) {
        Ok(Some(local)) => (local, None),
        Ok(None) => (slug(hostname), None),
        Err(error) => (slug(hostname), Some(error)),
    }
}
pub fn host_id(path: &Path, hostname: &str) -> String {
    resolve(path, hostname).0
}

pub fn peer_ids(path: &Path) -> BTreeSet<String> {
    fs::read(path)
        .ok()
        .and_then(|raw| decode_bytes(&raw).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| {
            v.get("peers")
                .and_then(Value::as_object)
                .map(|p| p.keys().cloned().collect())
        })
        .unwrap_or_default()
}

pub fn aliases(current: Option<&str>, path: &Path, hostname: &str) -> BTreeSet<String> {
    let current = current
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| host_id(path, hostname));
    let peers = peer_ids(path);
    let mut known: BTreeSet<_> = [hostname.to_owned(), slug(hostname), FALLBACK.into()]
        .into_iter()
        .filter(|v| !v.is_empty() && !peers.contains(v))
        .collect();
    known.insert(current);
    known
}

pub fn canonical(value: Value, current: &str, known: &BTreeSet<String>) -> Value {
    if value.as_str().is_some_and(|v| known.contains(v)) {
        current.into()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hostname_policy() {
        for (raw, expected) in [
            ("Anes-MacBook-Pro.local", "anes-macbook-pro"),
            ("studio", "studio"),
            ("My Mac (2)", "my-mac-2"),
            ("--build__box--.example.com", "build-box"),
            ("___", "local"),
            (".local", "local"),
            ("", "local"),
            ("日本", "local"),
        ] {
            assert_eq!(slug(raw), expected);
            assert!(valid_name(&slug(raw)));
        }
        assert_eq!(slug(&"a".repeat(80)), "a".repeat(63));
        assert_eq!(slug(&format!("{}-b", "a".repeat(62))), "a".repeat(62));
    }
    #[test]
    fn configuration_and_fallback_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.json");
        assert_eq!(resolve(&path, "box.local").0, "box");
        assert_eq!(resolve(&path, "").0, "local");
        for local in [
            serde_json::json!("My Mac"),
            "Studio".into(),
            "studio.local".into(),
            "".into(),
            7.into(),
            Value::Null,
            "a".repeat(64).into(),
        ] {
            fs::write(&path, serde_json::json!({"local_host":local}).to_string()).unwrap();
            let (id, error) = resolve(&path, "box.local");
            assert_eq!(id, "box");
            assert_eq!(error.unwrap().code(), "invalid_config");
        }
        for raw in [
            "{broken",
            r#"{"local_host":"studio","local_host":"other"}"#,
            r#"{"local_host":"studio","peers":{"a":{"x":1,"x":2}}}"#,
        ] {
            fs::write(&path, raw).unwrap();
            assert!(matches!(configured(&path), Err(InvalidHost::Json { .. })));
        }
        fs::write(&path, r#"{"local_host":"studio","peers":{"Bad Name":{}}}"#).unwrap();
        assert_eq!(configured(&path).unwrap().as_deref(), Some("studio"));
    }
    #[test]
    fn peer_names_never_become_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.json");
        fs::write(
            &path,
            r#"{"local_host":"studio","peers":{"box":{},"local":{}}}"#,
        )
        .unwrap();
        let known = aliases(None, &path, "box.local");
        assert_eq!(known, BTreeSet::from(["studio".into(), "box.local".into()]));
        for value in [
            Value::Null,
            serde_json::json!(["studio"]),
            "box".into(),
            "local".into(),
        ] {
            assert_eq!(canonical(value.clone(), "studio", &known), value);
        }
        assert_eq!(canonical("box.local".into(), "studio", &known), "studio");
    }
    #[test]
    fn aliases_preserve_earlier_local_ids_and_lenient_peers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.json");
        fs::write(&path, r#"{"peers":{"old":{}},"peers":{"new":{}}}"#).unwrap();
        assert_eq!(peer_ids(&path), BTreeSet::from(["new".into()]));
        let known = aliases(Some("studio"), &path, "Anes-MacBook-Pro.local");
        for stored in [
            "studio",
            "local",
            "Anes-MacBook-Pro.local",
            "anes-macbook-pro",
        ] {
            assert_eq!(canonical(stored.into(), "studio", &known), "studio");
        }
        fs::write(&path, "broken").unwrap();
        assert!(peer_ids(&path).is_empty());
    }
}

#[cfg(test)]
mod differential {
    use super::*;
    #[test]
    fn python_host_policy_fixtures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.json");
        let Some(expected) = crate::test_support::python(
            "scripts/host_identity.py",
            r#"
p = pathlib.Path(sys.argv[3])
m.hostname = lambda: 'Box.local'
fixtures = ['{"local_host":"studio","peers":{"box":{},"local":{}}}', '{"local_host":"Bad"}', '{"local_host":"studio","peers":{"a":{"x":1,"x":2}}}', '{bad', '{"local_host":"solo","peers":{}}', '[]']
results = []
for fixture in fixtures:
    p.write_text(fixture)
    host,error = m.resolve(p)
    results.append([host,error,sorted(m.aliases(path=p))])
print(json.dumps(results))
"#,
            &[&path],
        ) else {
            return;
        };
        let fixtures = [
            r#"{"local_host":"studio","peers":{"box":{},"local":{}}}"#,
            r#"{"local_host":"Bad"}"#,
            r#"{"local_host":"studio","peers":{"a":{"x":1,"x":2}}}"#,
            "{bad",
            r#"{"local_host":"solo","peers":{}}"#,
            "[]",
        ];
        let actual: Vec<_> = fixtures
            .iter()
            .map(|fixture| {
                fs::write(&path, fixture).unwrap();
                let (host, error) = resolve(&path, "Box.local");
                serde_json::json!([
                    host,
                    error.map(|e| e.to_string()),
                    aliases(None, &path, "Box.local")
                ])
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}

#[cfg(test)]
mod remaining_parity {
    use super::*;
    #[test]
    #[ignore = "RP-1001: Python byte encodings and extended JSON values"]
    fn python_extended_host_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.json");
        let Some(fixtures) = crate::test_support::python(
            "scripts/host_identity.py",
            r#"
p=pathlib.Path(sys.argv[3])
raws=[b'\xef\xbb\xbf{"local_host":"studio"}', '{"local_host":"studio"}'.encode('utf-16'), b'{"local_host":"studio","extra":NaN}', b'{"local_host":"studio","extra":1e400}', b'{"local_host":"studio","extra":"\\udcff"}']
results=[]
for raw in raws:
    p.write_bytes(raw)
    results.append([list(raw),m.configured(p)])
print(json.dumps(results))
"#,
            &[&path],
        ) else {
            return;
        };
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for fixture in fixtures.as_array().unwrap() {
            let raw: Vec<u8> = serde_json::from_value(fixture[0].clone()).unwrap();
            fs::write(&path, raw).unwrap();
            actual.push(serde_json::json!(configured(&path).ok().flatten()));
            expected.push(fixture[1].clone());
        }
        assert_eq!(actual, expected);
    }
}

#[cfg(test)]
mod encoding_tests {
    use super::*;
    #[test]
    fn python_bytes_encoding_detection_and_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.json");
        let Some(fixtures) = crate::test_support::python(
            "scripts/host_identity.py",
            r#"
p=pathlib.Path(sys.argv[3]); results=[]
text='{"local_host":"studio","peers":{"box":{}},"extra":"é😀"}'
raws=[text.encode(e) for e in ['utf-8','utf-8-sig','utf-16','utf-16-le','utf-16-be','utf-32','utf-32-le','utf-32-be']]
raws += [b'\xff', b'\xff\xfe{', b'\x00\x00\xfe\xff\x00', b'[]', b'{}', b'{"local_host":"studio","x":1,"x":2}',b'\x00"',b'"\x00']
for raw in raws:
    p.write_bytes(raw)
    try: result=m.configured(p)
    except m.InvalidHost as e: result={'error':str(e)}
    results.append([list(raw),result,sorted(m.peer_ids(p))])
print(json.dumps(results))
"#,
            &[&path],
        ) else {
            return;
        };
        for fixture in fixtures.as_array().unwrap() {
            let bytes: Vec<u8> = serde_json::from_value(fixture[0].clone()).unwrap();
            fs::write(&path, bytes).unwrap();
            let actual = match configured(&path) {
                Ok(v) => serde_json::json!(v),
                Err(e) => serde_json::json!({"error":e.to_string()}),
            };
            assert_eq!(actual, fixture[1], "{}", fixture[0]);
            assert_eq!(serde_json::json!(peer_ids(&path)), fixture[2]);
        }
    }
}

#[cfg(test)]
mod slug_value_tests {
    use super::*;
    #[test]
    fn permissive_slug_inputs_follow_python_truth_and_str() {
        let values = serde_json::json!([
            null,
            false,
            true,
            0,
            0.0,
            -1,
            1e-5,
            [],
            {},
            [true],
            "İ_Box.local",
            "a'b",
            300
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("values.json");
        fs::write(&path, serde_json::to_vec(&values).unwrap()).unwrap();
        let Some(expected) = crate::test_support::python(
            "scripts/host_identity.py",
            r#"
print(json.dumps([m.slug(v) for v in json.loads(pathlib.Path(sys.argv[3]).read_text())]))
"#,
            &[&path],
        ) else {
            return;
        };
        let actual: Vec<_> = values.as_array().unwrap().iter().map(slug_value).collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}
