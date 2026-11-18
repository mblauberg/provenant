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
// Short values are too ambiguous for document-wide substring replacement.
const MIN_EXACT_BYTES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    PullRequest,
    IssueComment,
    FixtureExport,
    ArtifactExport,
    GraphPublication,
    PeerPublication,
    CrossHostTransport,
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
/// The disclosure boundary for every payload sent to another host.
/// A failure withholds the payload; local state is never changed.
pub struct CrossHostTransport<'a> {
    projector: &'a Projector,
}
impl<'a> CrossHostTransport<'a> {
    pub fn new(projector: &'a Projector) -> Self {
        Self { projector }
    }

    /// Project only the caller's declared free-text fields. Typed credentials
    /// found in one field protect copies in the other fields, never wire metadata.
    pub fn texts(&self, texts: &mut [&mut String]) -> Result<(), ProjectionError> {
        let mut secrets = self.projector.secrets.clone();
        for text in texts.iter() {
            visit_documents(text, &mut |value| collect(value, &mut secrets));
        }
        let projector = Projector::new(secrets)?;
        for text in texts {
            **text = String::from_utf8(
                projector
                    .project(Destination::CrossHostTransport, text.as_bytes())?
                    .into_bytes(),
            )
            .map_err(|_| ProjectionError::Encoding)?;
        }
        Ok(())
    }

    pub fn inspect_text<'b>(&self, bytes: &'b [u8]) -> Result<&'b str, ProjectionError> {
        let text = std::str::from_utf8(bytes).map_err(|_| ProjectionError::Encoding)?;
        if text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err(ProjectionError::Encoding);
        }
        Ok(text)
    }
}

impl Projector {
    pub fn new(secrets: impl IntoIterator<Item = Vec<u8>>) -> Result<Self, ProjectionError> {
        let mut secrets: Vec<_> = secrets
            .into_iter()
            .filter(|secret| secret.len() >= MIN_EXACT_BYTES && secret != MASK.as_bytes())
            .collect();
        // Reuse the primitive's bounded secret-set contract, not its heuristic
        // token detection. Unrelated secret-shaped prose must remain lossless.
        secrets.sort();
        secrets.dedup();
        provenant_redact::Redactor::new(secrets.clone()).map_err(|_| ProjectionError::SecretSet)?;
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
        let args: Vec<String> = arguments
            .into_iter()
            .map(|arg| arg.into_string().map_err(|_| ProjectionError::Encoding))
            .collect::<Result<_, _>>()?;
        if args.iter().any(|arg| {
            [
                "--editor",
                "--web",
                "--recover",
                "--fill",
                "--fill-first",
                "--fill-verbose",
            ]
            .iter()
            .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
        }) {
            return Err(ProjectionError::Operation);
        }
        // Learn typed values across title/body/fields before projecting each
        // argument, so copies in another field are also protected.
        let mut secrets = self.secrets.clone();
        for arg in &args {
            visit_documents(arg, &mut |value| collect(value, &mut secrets));
        }
        if let Some(bytes) = &input {
            let text = std::str::from_utf8(bytes).map_err(|_| ProjectionError::Encoding)?;
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
            if !matches!(
                flag.as_str(),
                "--body-file" | "--template" | "-T" | "--input"
            ) {
                let projected = self.project(destination, args[index].as_bytes())?;
                let masked = provenant_redact::known_shapes(projected.bytes())
                    .map_err(|_| ProjectionError::Bound)?;
                args[index] = String::from_utf8(masked).map_err(|_| ProjectionError::Encoding)?;
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
                _ => unreachable!(),
            };
            let output_flag = if matches!(flag.as_str(), "--template" | "-T") {
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
fn quote(byte: u8) -> bool {
    matches!(byte, b'"' | b'\'' | b'`')
}

fn word(c: char) -> bool {
    if c.is_alphanumeric() || c == '_' {
        return true;
    }
    if c.is_ascii() {
        return false;
    }
    // Unicode word continuations include marks, connectors and join controls.
    // https://docs.rs/regex/latest/regex/#perl-character-classes-unicode-friendly
    static WORD: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    let regex = WORD.get_or_init(|| regex::Regex::new(r"^\w$").ok());
    regex
        .as_ref()
        .is_some_and(|re| re.is_match(c.encode_utf8(&mut [0; 4])))
}

fn quoted_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i = (i + 2).min(bytes.len());
        } else if bytes[i] == quote {
            // A possessive or contraction inside prose does not close its quote.
            if quote == b'\''
                && text[..i].chars().next_back().is_some_and(word)
                && text[i + 1..].chars().next().is_some_and(word)
            {
                i += 1;
                continue;
            }
            return Some(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

fn assignments(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    // Structured documents were already projected by transform_documents.
    // Rescanning their encoded strings could consume a JSON closing quote.
    let docs: Vec<_> = documents(text)
        .into_iter()
        .map(|(start, end, _)| (start, end))
        .collect();
    let mut doc = 0;
    let mut quotes = Vec::new();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        while quotes.last().is_some_and(|end| i >= *end) {
            if quotes.pop() == Some(i) {
                i += 1;
            }
        }
        if i >= bytes.len() {
            break;
        }
        while doc < docs.len() && i >= docs[doc].1 {
            doc += 1;
        }
        if doc < docs.len() && i == docs[doc].0 {
            i = docs[doc].1;
            continue;
        }
        let limit = quotes.last().copied().unwrap_or(bytes.len());
        let Some(c) = text[i..].chars().next() else {
            break;
        };
        if c.is_whitespace() || b",;{}[]()=:".contains(&bytes[i]) {
            i += c.len_utf8();
            continue;
        }
        let start = i;
        let key = if quote(bytes[i]) {
            let end = quoted_end(&text[..limit], i);
            let mut after = end.unwrap_or(limit);
            while after < limit && matches!(bytes[after], b' ' | b'\t') {
                after += 1;
            }
            if !end.is_some_and(|_| after < limit && matches!(bytes[after], b'=' | b':')) {
                // Inspect prose within quotes while keeping its closing quote
                // outside any unquoted assignment value. Use a stack, not recursion.
                if let Some(end) = end {
                    quotes.push(end - 1);
                }
                i += 1;
                continue;
            }
            let Some(end_idx) = end else {
                i += 1;
                continue;
            };
            i = end_idx;
            let raw = &text[start..i];
            std::borrow::Cow::Owned(
                serde_json::from_str::<String>(raw)
                    .unwrap_or_else(|_| raw[1..raw.len() - 1].to_owned()),
            )
        } else {
            // Canonicalise the whole bare name; punctuation must not expose
            // a credential-looking suffix. Delimiters require a quoted key.
            while i < limit {
                let Some(c) = text[i..].chars().next() else {
                    break;
                };
                if c.is_whitespace() || b",;{}[]()=:".contains(&bytes[i]) {
                    break;
                }
                i += c.len_utf8();
            }
            std::borrow::Cow::Borrowed(&text[start..i])
        };
        if !secret_field(&key) {
            continue;
        }
        while i < limit && matches!(bytes[i], b' ' | b'\t' | b'"' | b'\'') {
            i += 1;
        }
        if i == limit || !matches!(bytes[i], b'=' | b':') {
            continue;
        }
        i += 1;
        while i < limit && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        let start = i;
        if doc < docs.len() && i == docs[doc].0 {
            i = docs[doc].1;
        } else if i < limit && quote(bytes[i]) {
            i = quoted_end(&text[..limit], i).unwrap_or(limit);
        } else if provenant_redact::is_line_assignment(&key) {
            while i < limit && !matches!(bytes[i], b'\n' | b'\r') {
                i += 1;
            }
        } else {
            while i < limit && !bytes[i].is_ascii_whitespace() && !b",;}".contains(&bytes[i]) {
                i += 1;
            }
        }
        if i > start {
            spans.push((start, i));
        }
    }
    spans
}
fn mask_assignments(text: &str) -> String {
    let mut out = String::new();
    let mut offset = 0;
    for (start, end) in assignments(text) {
        out.push_str(&text[offset..start]);
        if quote(text.as_bytes()[start]) {
            let delimiter = char::from(text.as_bytes()[start]);
            out.push(delimiter);
            out.push_str(MASK);
            out.push(delimiter);
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
        // Short typed/ambient values never become global substring masks.
        let short = Projector::new([b"it".to_vec()]).unwrap();
        let short_raw = br#"{"token":"it","it":"it","message":"token: it; it stays here"}"#;
        let projected = short.json(Destination::PeerPublication, short_raw).unwrap();
        let value: Value = serde_json::from_slice(projected.bytes()).unwrap();
        assert_eq!(value["token"], MASK);
        assert_eq!(value["it"], "it");
        assert_eq!(value["message"], "token: [REDACTED]; it stays here");
        let temp = tempfile::tempdir().unwrap();
        let content = br#"{"ordinary":"ordinary"}"#;
        std::fs::write(temp.path().join("--field"), content).unwrap();
        let operation = Projector::new([])
            .unwrap()
            .forge(
                ForgeOperation::Cli {
                    arguments: ["api", "--input", "--field", "token=ordinary"]
                        .map(OsString::from)
                        .into(),
                    input: None,
                },
                temp.path(),
            )
            .unwrap();
        let ForgeOperation::Cli {
            input: Some(input), ..
        } = operation
        else {
            panic!("projected input")
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&input).unwrap(),
            serde_json::from_slice::<Value>(content).unwrap()
        );
        projection.bytes.push(b'!');
        assert!(!projection.matches_source(Destination::PeerPublication, raw));
    }

    // Spec: docs/specs/fabric.md#publication-boundary
    #[test]
    fn publication_masks_assignments_only_in_their_spans() {
        let projector = Projector::new([b"explicit-value".to_vec()]).unwrap();
        let raw =
            b"password=private-value; copy private-value, explicit-value; ordinary ghp_unrelated";
        let projection = projector
            .project(Destination::GraphPublication, raw)
            .unwrap();
        let output = std::str::from_utf8(projection.bytes()).unwrap();
        assert_eq!(
            output,
            "password=[REDACTED]; copy private-value, [REDACTED]; ordinary ghp_unrelated"
        );
        assert!(!output.contains("explicit-value"));
        assert!(output.contains("ordinary ghp_unrelated"));
        let prose = b"token: ordinary; ordinary stays outside its span, as does it";
        let projected = projector
            .project(Destination::GraphPublication, prose)
            .unwrap();
        assert_eq!(
            projected.bytes(),
            b"token: [REDACTED]; ordinary stays outside its span, as does it"
        );
        for (raw, expected) in [
            (
                "api-key=private-value; copy private-value",
                "api-key=[REDACTED]; copy private-value",
            ),
            (
                "X-API-Key: private-value; copy private-value",
                "X-API-Key: [REDACTED]; copy private-value",
            ),
            (
                "api.key=private-value; copy private-value",
                "api.key=[REDACTED]; copy private-value",
            ),
            (
                "aws.secret.access.key=private-value; copy private-value",
                "aws.secret.access.key=[REDACTED]; copy private-value",
            ),
            (
                "api+key=private-value; copy private-value",
                "api+key=[REDACTED]; copy private-value",
            ),
            (
                "api/key=private-value; copy private-value",
                "api/key=[REDACTED]; copy private-value",
            ),
            (
                r#""api key"="private-value"; copy private-value"#,
                r#""api key"="[REDACTED]"; copy private-value"#,
            ),
            (
                r#""api,key": private-value; copy private-value"#,
                r#""api,key": [REDACTED]; copy private-value"#,
            ),
            (
                r#""api\u002ekey"=private-value; copy private-value"#,
                r#""api\u002ekey"=[REDACTED]; copy private-value"#,
            ),
            (
                "'user's api key'='private'value'; copy private'value",
                "'user's api key'='[REDACTED]'; copy private'value",
            ),
            (
                "it's token=private'value; copy private'value",
                "it's token=[REDACTED]; copy private'value",
            ),
            (
                "not-a-token=ordinary; 7token=ordinary; not.a.token=ordinary; not/token=ordinary; not+token=ordinary",
                "not-a-token=ordinary; 7token=ordinary; not.a.token=ordinary; not/token=ordinary; not+token=ordinary",
            ),
        ] {
            let projected = projector
                .project(Destination::CrossHostTransport, raw.as_bytes())
                .unwrap();
            assert_eq!(projected.bytes(), expected.as_bytes());
        }
        let quoted_sentence = projector
            .project(
                Destination::CrossHostTransport,
                br#"diagnostic "token: private-value"; copy private-value"#,
            )
            .unwrap();
        let quoted_sentence = std::str::from_utf8(quoted_sentence.bytes()).unwrap();
        assert_eq!(
            quoted_sentence,
            r#"diagnostic "token: [REDACTED]"; copy private-value"#
        );
        for (raw, expected) in [
            (
                "diagnostic 'token: private-value'; copy private-value",
                "diagnostic 'token: [REDACTED]'; copy private-value",
            ),
            (
                "diagnostic 'the user's token: private-value'; copy private-value",
                "diagnostic 'the user's token: [REDACTED]'; copy private-value",
            ),
            (
                "diagnostic 'it isn't safe: token: private-value'; copy private-value",
                "diagnostic 'it isn't safe: token: [REDACTED]'; copy private-value",
            ),
            (
                "diagnostic `token: private-value`; copy private-value",
                "diagnostic `token: [REDACTED]`; copy private-value",
            ),
            (
                "`api key`=`private-value`; copy private-value",
                "`api key`=`[REDACTED]`; copy private-value",
            ),
        ] {
            let projected = projector
                .project(Destination::CrossHostTransport, raw.as_bytes())
                .unwrap();
            assert_eq!(projected.bytes(), expected.as_bytes());
        }
        for continuation in [
            "\u{301}", "\u{903}", "\u{20DD}", "\u{203F}", "\u{200C}", "\u{200D}", "\u{B2}",
        ] {
            let raw = format!(
                "diagnostic 'cafe{continuation}'s token: private-value'; copy private-value"
            );
            let expected =
                format!("diagnostic 'cafe{continuation}'s token: [REDACTED]'; copy private-value");
            let projected = projector
                .project(Destination::CrossHostTransport, raw.as_bytes())
                .unwrap();
            assert_eq!(projected.bytes(), expected.as_bytes());
        }
        let json_prose = br#"{"message":"ordinary stays; token: ordinary","ordinary":"ordinary","tag":"submit","id":"submit-test"}"#;
        let projected = projector
            .project(Destination::CrossHostTransport, json_prose)
            .unwrap();
        let mut expected: Value = serde_json::from_slice(json_prose).unwrap();
        expected["message"] = Value::String("ordinary stays; token: [REDACTED]".into());
        assert_eq!(
            serde_json::from_slice::<Value>(projected.bytes()).unwrap(),
            expected
        );
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

    #[test]
    fn unquoted_authorization_header_value_masks_scheme_and_token() {
        let projector = Projector::new([]).unwrap();
        let cases = [
            (
                "Authorization: Bearer secret-token\nother: safe\n",
                "Authorization: [REDACTED]\nother: safe\n",
            ),
            (
                "authorization: Basic dXNlcjpwYXNz\nnext: value\n",
                "authorization: [REDACTED]\nnext: value\n",
            ),
            (
                "Authorization: Token secret-token",
                "Authorization: [REDACTED]",
            ),
            (
                "Proxy-Authorization: Negotiate kerberos-token\n",
                "Proxy-Authorization: [REDACTED]\n",
            ),
        ];
        for (raw, expected) in cases {
            let projected = projector
                .project(Destination::GraphPublication, raw.as_bytes())
                .unwrap();
            assert_eq!(std::str::from_utf8(projected.bytes()).unwrap(), expected);
        }
    }

    #[test]
    fn cli_arguments_are_projected_without_splitting() {
        let token = "ghp_abcdefghijklmnopqrst";
        let arguments = [
            "pr",
            "review",
            "-r",
            "--body",
            token,
            "--subject",
            "-bump deps",
        ]
        .map(OsString::from)
        .into();
        let ForgeOperation::Cli { arguments, .. } = Projector::new([])
            .unwrap()
            .forge(
                ForgeOperation::Cli {
                    arguments,
                    input: None,
                },
                Path::new("."),
            )
            .unwrap()
        else {
            panic!("expected CLI operation")
        };
        let arguments: Vec<_> = arguments.iter().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(
            arguments,
            [
                "pr",
                "review",
                "-r",
                "--body",
                "[REDACTED]",
                "--subject",
                "-bump deps"
            ]
        );
    }
}
