//! Destination-owned disclosure. Private inputs are never edited. Only a fully
//! projected, byte-bound payload is returned to a publisher.
use provenant_config::{ForgeOperation, PortError};
use provenant_records::{CanonicalV1, FileDigest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    ffi::OsString,
    io::Read,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

const LIMIT: usize = 16 * 1024 * 1024;
const MASK: &str = "[REDACTED]";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    PullRequest,
    IssueComment,
    FixtureExport,
    ArtifactExport,
    GraphPublication,
    PeerPublication,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadBinding {
    pub bytes: u64,
    pub sha256: FileDigest,
}
impl PayloadBinding {
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.len() as u64,
            sha256: CanonicalV1::file_digest(bytes),
        }
    }
}

/// Metadata describes the exact bytes returned by `bytes`, separately from the
/// original. Debug intentionally omits payload contents.
pub struct Projection {
    pub destination: Destination,
    pub raw: PayloadBinding,
    pub published: PayloadBinding,
    bytes: Vec<u8>,
}
impl Projection {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
    pub fn matches_source(&self, destination: Destination, raw: &[u8]) -> bool {
        destination == self.destination
            && self.raw == PayloadBinding::of(raw)
            && self.published == PayloadBinding::of(&self.bytes)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error("invalid publication secret set")]
    SecretSet,
    #[error("publication payload exceeds its projection bound")]
    Bound,
    #[error("publication payload is not UTF-8")]
    Encoding,
    #[error("publication document is malformed")]
    Document,
    #[error("publication input file cannot be read")]
    Input,
    #[error("publication operation cannot be projected safely")]
    Operation,
}

/// Central declaration for typed credential fields and credential environment
/// values. Matching a field name is distinct from guessing at a value's shape.
pub fn secret_field(name: &str) -> bool {
    let name: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        name.as_str(),
        "secret"
            | "password"
            | "passwd"
            | "pgpassword"
            | "token"
            | "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
            | "apikey"
            | "privatekey"
            | "clientsecret"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "credentials"
            | "credential"
            | "awsaccesskeyid"
            | "awssecretaccesskey"
            | "awssessiontoken"
            | "ghauthtoken"
            | "ghtoken"
            | "githubtoken"
            | "claudecodeoauthtoken"
    ) || [
        "apikey",
        "password",
        "authtoken",
        "accesstoken",
        "refreshtoken",
        "secret",
        "secretkey",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
}

pub struct Projector {
    secrets: Vec<Vec<u8>>,
}
impl Projector {
    pub fn new(secrets: impl IntoIterator<Item = Vec<u8>>) -> Result<Self, ProjectionError> {
        let mut secrets: Vec<_> = secrets.into_iter().collect();
        // Reuse the primitive's bounded secret-set contract, not its heuristic
        // token detection. Unrelated secret-shaped prose must remain lossless.
        secrets.sort();
        secrets.dedup();
        crate::Redactor::new(secrets.clone()).map_err(|_| ProjectionError::SecretSet)?;
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Ok(Self { secrets })
    }

    pub fn from_environment(
        environment: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Self, ProjectionError> {
        Self::new(
            environment
                .into_iter()
                .filter(|(key, value)| key.to_str().is_some_and(secret_field) && !value.is_empty())
                .map(|(_, value)| value.as_bytes().to_vec()),
        )
    }

    pub fn ambient() -> Result<Self, ProjectionError> {
        Self::from_environment(std::env::vars_os())
    }

    pub fn project(
        &self,
        destination: Destination,
        bytes: &[u8],
    ) -> Result<Projection, ProjectionError> {
        if bytes.len() > LIMIT {
            return Err(ProjectionError::Bound);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ProjectionError::Encoding)?;
        let mut secrets = self.secrets.clone();
        // Discover typed values before masking, including copies elsewhere in
        // the same document. Decode JSON escapes before exact-value matching.
        visit_documents(text, &mut |value| collect(value, &mut secrets));
        collect_assignments(text, &mut secrets);
        let projector = Self::new(secrets)?;
        let mut out = transform_documents(text, &projector)?;
        out = mask_assignments(&out);
        let bytes_out = projector.exact(out.as_bytes());
        Ok(Projection {
            destination,
            raw: PayloadBinding::of(bytes),
            published: PayloadBinding::of(&bytes_out),
            bytes: bytes_out,
        })
    }

    /// Strict structured export: malformed JSON is withheld, never treated as
    /// an unstructured fallback. All decoded strings and object keys are masked.
    pub fn json(
        &self,
        destination: Destination,
        bytes: &[u8],
    ) -> Result<Projection, ProjectionError> {
        if bytes.len() > LIMIT {
            return Err(ProjectionError::Bound);
        }
        let mut value: Value =
            serde_json::from_slice(bytes).map_err(|_| ProjectionError::Document)?;
        let mut secrets = self.secrets.clone();
        collect(&value, &mut secrets);
        let projector = Self::new(secrets)?;
        projector.value(&mut value)?;
        let projected = serde_json::to_vec(&value).map_err(|_| ProjectionError::Document)?;
        Ok(Projection {
            destination,
            raw: PayloadBinding::of(bytes),
            published: PayloadBinding::of(&projected),
            bytes: projected,
        })
    }

    fn exact(&self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut offset = 0;
        while offset < bytes.len() {
            if let Some(secret) = self.secrets.iter().find(|s| bytes[offset..].starts_with(s)) {
                out.extend_from_slice(MASK.as_bytes());
                offset += secret.len();
            } else {
                out.push(bytes[offset]);
                offset += 1;
            }
        }
        out
    }

    fn value(&self, value: &mut Value) -> Result<(), ProjectionError> {
        match value {
            Value::String(text) => {
                let nested = transform_documents(text, self)?;
                let nested = mask_assignments(&nested);
                *text = String::from_utf8(self.exact(nested.as_bytes()))
                    .map_err(|_| ProjectionError::Encoding)?;
            }
            Value::Array(items) => {
                for item in items {
                    self.value(item)?;
                }
            }
            Value::Object(map) => {
                let entries = std::mem::take(map);
                for (key, mut item) in entries {
                    if secret_field(&key) {
                        item = Value::String(MASK.into());
                    } else {
                        self.value(&mut item)?;
                    }
                    let key = String::from_utf8(self.exact(key.as_bytes()))
                        .map_err(|_| ProjectionError::Encoding)?;
                    // Redacting keys must never silently drop a field.
                    if map.insert(key, item).is_some() {
                        return Err(ProjectionError::Document);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Projection runs before the common forge adapter chooses either backend.
    /// File inputs are read once and replaced by the projected in-memory bytes.
    pub fn forge(
        &self,
        operation: ForgeOperation,
        project: &Path,
    ) -> Result<ForgeOperation, ProjectionError> {
        let destination = match &operation {
            ForgeOperation::Open { .. } => Destination::PullRequest,
            _ => Destination::IssueComment,
        };
        Ok(match operation {
            ForgeOperation::Open {
                base,
                head,
                title,
                body,
                draft,
            } => {
                // Title and body share one exact secret inventory.
                let source = serde_json::to_vec(&serde_json::json!({"title":title,"body":std::str::from_utf8(&body).map_err(|_| ProjectionError::Encoding)?}))
                    .map_err(|_| ProjectionError::Document)?;
                let payload = self.json(destination, &source)?;
                let mut value: Value = serde_json::from_slice(payload.bytes())
                    .map_err(|_| ProjectionError::Document)?;
                ForgeOperation::Open {
                    base,
                    head,
                    title: value["title"]
                        .take()
                        .as_str()
                        .ok_or(ProjectionError::Document)?
                        .into(),
                    body: self
                        .project(
                            destination,
                            value["body"]
                                .as_str()
                                .ok_or(ProjectionError::Document)?
                                .as_bytes(),
                        )?
                        .into_bytes(),
                    draft,
                }
            }
            ForgeOperation::Land { intent } => ForgeOperation::Land {
                intent: self
                    .json(Destination::ArtifactExport, &intent)?
                    .into_bytes(),
            },
            ForgeOperation::Cli { arguments, input } => {
                let (arguments, input) = self.arguments(arguments, input, project, destination)?;
                ForgeOperation::Cli { arguments, input }
            }
            ForgeOperation::Api {
                path,
                arguments,
                etag,
            } => {
                let (arguments, input) = self.arguments(arguments, None, project, destination)?;
                // This operation has no stdin contract. Never leave a file
                // reference for a publisher to reopen after projection.
                if input.is_some() {
                    return Err(ProjectionError::Operation);
                }
                ForgeOperation::Api {
                    path,
                    arguments,
                    etag,
                }
            }
            other => other,
        })
    }

    fn arguments(
        &self,
        arguments: Vec<OsString>,
        input: Option<Vec<u8>>,
        project: &Path,
        destination: Destination,
    ) -> Result<(Vec<OsString>, Option<Vec<u8>>), ProjectionError> {
        let mut args = Vec::new();
        for arg in arguments {
            let arg = arg.into_string().map_err(|_| ProjectionError::Encoding)?;
            if [
                "--editor",
                "--web",
                "--recover",
                "--fill",
                "--fill-first",
                "--fill-verbose",
            ]
            .iter()
            .any(|flag| arg == *flag || arg.starts_with(&format!("{flag}=")))
            {
                // These ask gh to obtain content outside the snapshot. The
                // caller must supply the body explicitly for publication.
                return Err(ProjectionError::Operation);
            }
            if arg.len() > 2
                && ["-b", "-t", "-f", "-F"]
                    .iter()
                    .any(|flag| arg.starts_with(flag))
                && !arg.starts_with("--")
            {
                args.push(arg[..2].to_owned());
                args.push(arg[2..].to_owned());
            } else {
                args.push(arg);
            }
        }
        // Learn typed values across title/body/fields before projecting each
        // argument, so copies in another field are also protected.
        let mut secrets = self.secrets.clone();
        for arg in &args {
            collect_assignments(arg, &mut secrets);
            visit_documents(arg, &mut |value| collect(value, &mut secrets));
        }
        if let Some(bytes) = &input {
            let text = std::str::from_utf8(bytes).map_err(|_| ProjectionError::Encoding)?;
            collect_assignments(text, &mut secrets);
            visit_documents(text, &mut |value| collect(value, &mut secrets));
        }
        let projector = Self::new(secrets)?;
        projector.project_arguments(args, input, project, destination)
    }

    fn project_arguments(
        &self,
        mut args: Vec<String>,
        mut input: Option<Vec<u8>>,
        project: &Path,
        destination: Destination,
    ) -> Result<(Vec<OsString>, Option<Vec<u8>>), ProjectionError> {
        if matches!(args.first().map(String::as_str), Some("pr" | "issue"))
            && matches!(
                args.get(1).map(String::as_str),
                Some("create" | "comment" | "review")
            )
            && !args.iter().any(|arg| {
                matches!(
                    arg.split('=').next().unwrap_or(""),
                    "--body" | "-b" | "--body-file" | "--template" | "-T"
                )
            })
        {
            return Err(ProjectionError::Operation);
        }
        let mut index = 0;
        while index < args.len() {
            let (flag, inline) = args[index]
                .split_once('=')
                .map_or((args[index].as_str(), None), |(k, v)| (k, Some(v)));
            let flag = flag.to_owned();
            let takes_value = matches!(
                flag.as_str(),
                "--body"
                    | "-b"
                    | "--title"
                    | "-t"
                    | "--body-file"
                    | "--template"
                    | "-T"
                    | "-F"
                    | "--field"
                    | "-f"
                    | "--raw-field"
                    | "--input"
            );
            if !takes_value {
                if matches!(flag.as_str(), "-H" | "--header") {
                    if args.get(index + 1).is_none() {
                        return Err(ProjectionError::Operation);
                    }
                    index += 2;
                    continue;
                }
                args[index] = String::from_utf8(self.exact(args[index].as_bytes()))
                    .map_err(|_| ProjectionError::Encoding)?;
                index += 1;
                continue;
            }
            let inline = inline.map(str::to_owned);
            let value_index = if inline.is_some() { index } else { index + 1 };
            let value = inline
                .clone()
                .or_else(|| args.get(value_index).cloned())
                .ok_or(ProjectionError::Operation)?;
            let projected = match flag.as_str() {
                "--body-file" | "--template" | "-T" | "--input" => {
                    let bytes = if value == "-" {
                        input.take().ok_or(ProjectionError::Input)?
                    } else {
                        read_input(project, &value)?
                    };
                    let projection = if flag == "--input" {
                        self.json(destination, &bytes)?
                    } else {
                        self.project(destination, &bytes)?
                    };
                    input = Some(projection.into_bytes());
                    "-".to_owned()
                }
                "-f" | "--raw-field" | "-F" | "--field" => {
                    let (key, val) = value.split_once('=').ok_or(ProjectionError::Operation)?;
                    let bytes = if matches!(flag.as_str(), "-F" | "--field") && val.starts_with('@')
                    {
                        if val == "@-" {
                            input.take().ok_or(ProjectionError::Input)?
                        } else {
                            read_input(project, &val[1..])?
                        }
                    } else {
                        val.as_bytes().to_vec()
                    };
                    let projected = if secret_field(key) {
                        MASK.as_bytes().to_vec()
                    } else {
                        self.project(destination, &bytes)?.into_bytes()
                    };
                    format!(
                        "{key}={}",
                        String::from_utf8(projected).map_err(|_| ProjectionError::Encoding)?
                    )
                }
                _ => String::from_utf8(self.project(destination, value.as_bytes())?.into_bytes())
                    .map_err(|_| ProjectionError::Encoding)?,
            };
            // -F/--field values beginning @ are interpreted as file references
            // by gh. Convert to raw fields after snapshotting, even if the
            // projected content itself begins with @.
            let output_flag = if matches!(flag.as_str(), "-F" | "--field") {
                "--raw-field"
            } else if matches!(flag.as_str(), "--template" | "-T") {
                "--body-file"
            } else {
                &flag
            };
            if inline.is_some() {
                args[index] = format!("{output_flag}={projected}");
            } else {
                args[index] = output_flag.into();
                args[value_index] = projected;
            }
            index = value_index + 1;
        }
        if let Some(bytes) = input.take() {
            input = Some(self.project(destination, &bytes)?.into_bytes());
        }
        Ok((args.into_iter().map(OsString::from).collect(), input))
    }
}

pub fn install_forge(
    forge: &mut provenant_config::ForgePort,
    project: PathBuf,
    projector: Result<Projector, ProjectionError>,
) {
    forge.set_publication_projector(move |operation| {
        projector
            .as_ref()
            .map_err(|_| PortError::Projection)?
            .forge(operation, &project)
            .map_err(|_| PortError::Projection)
    });
}

fn read_input(project: &Path, path: &str) -> Result<Vec<u8>, ProjectionError> {
    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        project.join(path)
    };
    let mut file = std::fs::File::open(path).map_err(|_| ProjectionError::Input)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ProjectionError::Input)?;
    if bytes.len() > LIMIT {
        return Err(ProjectionError::Bound);
    }
    Ok(bytes)
}

fn collect(value: &Value, secrets: &mut Vec<Vec<u8>>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                if secret_field(key) {
                    collect_strings(item, secrets);
                } else {
                    collect(item, secrets);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect(item, secrets);
            }
        }
        Value::String(text) => {
            visit_documents(text, &mut |value| collect(value, secrets));
            collect_assignments(text, secrets);
        }
        _ => {}
    }
}
fn collect_strings(value: &Value, secrets: &mut Vec<Vec<u8>>) {
    match value {
        Value::String(s) if !s.is_empty() && s != MASK => secrets.push(s.as_bytes().to_vec()),
        Value::Object(map) => {
            for value in map.values() {
                collect_strings(value, secrets);
            }
        }
        Value::Array(items) => {
            for value in items {
                collect_strings(value, secrets);
            }
        }
        _ => {}
    }
}

// Find complete JSON documents embedded in Markdown, JSONL or ordinary prose.
// A malformed structured export uses `json` and is withheld instead.
fn documents(text: &str) -> Vec<(usize, usize, Value)> {
    let mut docs = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        let Some(start) = text[offset..].find(['{', '[']) else {
            break;
        };
        let start = offset + start;
        let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        if let Some(Ok(value)) = stream.next() {
            let end = start + stream.byte_offset();
            docs.push((start, end, value));
            offset = end;
        } else {
            offset = start + 1;
        }
    }
    docs
}
fn visit_documents(text: &str, visit: &mut impl FnMut(&Value)) {
    for (_, _, value) in documents(text) {
        visit(&value);
    }
}
fn transform_documents(text: &str, projector: &Projector) -> Result<String, ProjectionError> {
    let mut out = String::new();
    let mut offset = 0;
    for (start, end, mut value) in documents(text) {
        out.push_str(&text[offset..start]);
        let before = value.clone();
        projector.value(&mut value)?;
        if before == value {
            out.push_str(&text[start..end]);
        } else {
            out.push_str(&serde_json::to_string(&value).map_err(|_| ProjectionError::Document)?);
        }
        offset = end;
    }
    out.push_str(&text[offset..]);
    Ok(out)
}

// Typed assignments are useful for prose diagnostics and shell/config snippets;
// only a declared credential key consumes its value. Other prose is untouched.
fn assignments(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        if !secret_field(&text[start..i]) {
            continue;
        }
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'"' | b'\'') {
            i += 1;
        }
        if i == bytes.len() || !matches!(bytes[i], b'=' | b':') {
            continue;
        }
        i += 1;
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        let start = i;
        if i < bytes.len() && matches!(bytes[i], b'"' | b'\'') {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else {
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !b",;}".contains(&bytes[i])
            {
                i += 1;
            }
        }
        if i > start {
            spans.push((start, i));
        }
    }
    spans
}
fn collect_assignments(text: &str, secrets: &mut Vec<Vec<u8>>) {
    for (start, end) in assignments(text) {
        let raw = &text[start..end];
        let value = serde_json::from_str::<String>(raw)
            .unwrap_or_else(|_| raw.trim_matches(['\'', '"']).into());
        if !value.is_empty() && value != MASK {
            secrets.push(value.into_bytes());
        }
    }
}
fn mask_assignments(text: &str) -> String {
    let mut out = String::new();
    let mut offset = 0;
    for (start, end) in assignments(text) {
        out.push_str(&text[offset..start]);
        if text.as_bytes()[start] == b'"' {
            out.push('"');
            out.push_str(MASK);
            out.push('"');
        } else if text.as_bytes()[start] == b'\'' {
            out.push('\'');
            out.push_str(MASK);
            out.push('\'');
        } else {
            out.push_str(MASK);
        }
        offset = end;
    }
    out.push_str(&text[offset..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec: docs/specs/fabric.md#publication-boundary
    #[test]
    fn publication_masks_typed_and_exact_values_and_binds_both_payloads() {
        let projector = Projector::new([b"known-secret".to_vec()]).unwrap();
        let raw = br#"{"token":"hidden-value","copy":"hidden-value","message":"known-secret and ordinary text"}"#;
        let mut projection = projector.json(Destination::PeerPublication, raw).unwrap();
        let output = std::str::from_utf8(projection.bytes()).unwrap();
        assert!(!output.contains("hidden-value"));
        assert!(!output.contains("known-secret"));
        assert!(output.contains("ordinary text"));
        assert_eq!(projection.raw, PayloadBinding::of(raw));
        assert_eq!(projection.published, PayloadBinding::of(projection.bytes()));
        assert_ne!(projection.raw, projection.published);
        assert!(projection.matches_source(Destination::PeerPublication, raw));
        assert!(!projection.matches_source(Destination::GraphPublication, raw));
        assert!(!projection.matches_source(Destination::PeerPublication, b"different"));
        projection.bytes.push(b'!');
        assert!(!projection.matches_source(Destination::PeerPublication, raw));
    }

    // Spec: docs/specs/fabric.md#publication-boundary
    #[test]
    fn publication_masks_typed_assignments_and_preserves_unrelated_text() {
        let projector = Projector::new([b"explicit-value".to_vec()]).unwrap();
        let raw =
            b"password=private-value; copy private-value, explicit-value; ordinary ghp_unrelated";
        let projection = projector
            .project(Destination::GraphPublication, raw)
            .unwrap();
        let output = std::str::from_utf8(projection.bytes()).unwrap();
        assert!(!output.contains("private-value"));
        assert!(!output.contains("explicit-value"));
        assert!(output.contains("ordinary ghp_unrelated"));
        assert_eq!(projection.raw.bytes, raw.len() as u64);
        assert_eq!(projection.published.bytes, projection.bytes().len() as u64);
    }

    // Spec: docs/specs/fabric.md#publication-boundary
    #[test]
    fn publication_failure_withholds_invalid_external_payload() {
        let projector = Projector::new([]).unwrap();
        assert!(matches!(
            projector.project(Destination::PeerPublication, b"\xff"),
            Err(ProjectionError::Encoding)
        ));
        assert!(matches!(
            projector.json(Destination::GraphPublication, b"{invalid"),
            Err(ProjectionError::Document)
        ));
        let operation = ForgeOperation::Open {
            base: "main".into(),
            head: "branch".into(),
            title: "title".into(),
            body: vec![0xff],
            draft: false,
        };
        assert!(matches!(
            projector.forge(operation, Path::new(".")),
            Err(ProjectionError::Encoding)
        ));
    }
}
