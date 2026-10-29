//! Session hand-off wire types. A hand-off is a fresh session started from a
//! prompt on a peer, never a transcript transfer. One request carries the
//! prompt, the branches to recreate and the attachments, so a key restricted to
//! `fabric peer` keeps working and no second channel is needed.
use provenant_core::OperationId;
use serde::{Deserialize, Serialize};

pub mod session;

/// Total decoded attachment bytes. Base64 inflates this to about 683 KiB,
/// which leaves room for the prompt inside the 1 MiB peer frame.
pub const ATTACHMENT_LIMIT: usize = 512 * 1024;
pub const ATTACHMENT_COUNT: usize = 32;
/// Matches the session checkpoint limit, so the prompt is also the checkpoint.
pub const PROMPT_LIMIT: usize = 32 * 1024;
pub const BRANCH_COUNT: usize = 64;
pub const ACCOUNT_LIMIT: usize = 64;
pub const MODEL_LIMIT: usize = 128;
/// Every hand-off prompt starts with this line, so the stored prompt can never
/// be read as a `claude` option.
pub const PROMPT_HEADER: &str = "# Session hand-off";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHandoff {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    /// Names the target's state directory; a replay never launches twice.
    pub operation_id: OperationId,
    /// Registered Claude account label on the target.
    pub account: String,
    pub model: Option<String>,
    pub prompt: String,
    /// Pushed branches the target recreates from `origin/<branch>`.
    pub branches: Vec<String>,
    pub attachments: Vec<Attachment>,
    /// False delivers everything but opens no Terminal.
    pub launch: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    /// A plain file name; the target chooses the directory.
    pub name: String,
    /// Standard padded base64 of the file bytes.
    pub base64: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum HandoffReceipt {
    Delivered {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        /// Absolute path of the stored prompt on the target.
        prompt_path: String,
        project: String,
        worktrees: Vec<HandoffWorktree>,
        attachments: Vec<String>,
        /// The launch script, when one was written.
        script_path: Option<String>,
        launched: bool,
        launch_error: Option<String>,
    },
    Refused {
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
        code: String,
        message: String,
        /// Registered Claude accounts on the target, for an account refusal.
        accounts: Vec<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffWorktree {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub branch: String,
    pub path: Option<String>,
    pub head: Option<String>,
    pub created: bool,
    pub error: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HandoffError {
    #[error("hand-off prompt must start with \"# Session hand-off\" and fit 32 KiB")]
    Prompt,
    #[error("account label is invalid")]
    Account,
    #[error("model name is invalid")]
    Model,
    #[error("at most 64 branches with valid names are allowed")]
    Branches,
    #[error("attachment {0:?} has an invalid or duplicate name")]
    AttachmentName(String),
    #[error("attachment {0:?} is not valid base64")]
    AttachmentEncoding(String),
    #[error("attachments exceed 32 files or 512 KiB in total")]
    AttachmentSize,
}

impl HandoffError {
    /// Stable identifier independent of the displayed explanation.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Account => "account",
            Self::Model => "model",
            Self::Branches => "branches",
            Self::AttachmentName(_) => "attachment_name",
            Self::AttachmentEncoding(_) => "attachment_encoding",
            Self::AttachmentSize => "attachment_size",
        }
    }
}

fn label(text: &str, limit: usize, extra: &[u8]) -> bool {
    !text.is_empty()
        && text.len() <= limit
        && text
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
}
/// A plain, portable file name with no directory part.
pub fn valid_attachment_name(name: &str) -> bool {
    label(name, 128, b"._- ") && !name.contains("..")
}
/// The same shape `git check-ref-format --branch` accepts for ordinary names,
/// kept narrow so a name is also safe in a prompt and a path.
pub fn valid_branch(name: &str) -> bool {
    label(name, 200, b"._-/")
        && !name.contains("..")
        && !name.contains("//")
        && !name.ends_with('/')
        && !name.ends_with(".lock")
        && !name.ends_with('.')
        && name != "HEAD"
}
pub fn valid_model(model: &str) -> bool {
    label(model, MODEL_LIMIT, b"._-/:[]")
}

impl SessionHandoff {
    /// Bounds checks shared by both ends, with the decoded attachments.
    pub fn validate(&self) -> Result<Vec<(String, Vec<u8>)>, HandoffError> {
        if !self.prompt.starts_with(PROMPT_HEADER) || self.prompt.len() > PROMPT_LIMIT {
            return Err(HandoffError::Prompt);
        }
        if !label(&self.account, ACCOUNT_LIMIT, b"._-") || self.account == "unknown" {
            return Err(HandoffError::Account);
        }
        if self.model.as_deref().is_some_and(|m| !valid_model(m)) {
            return Err(HandoffError::Model);
        }
        if self.branches.len() > BRANCH_COUNT
            || self.branches.iter().any(|b| !valid_branch(b))
            || (1..self.branches.len()).any(|i| self.branches[..i].contains(&self.branches[i]))
        {
            return Err(HandoffError::Branches);
        }
        if self.attachments.len() > ATTACHMENT_COUNT {
            return Err(HandoffError::AttachmentSize);
        }
        let mut total = 0usize;
        let mut decoded: Vec<(String, Vec<u8>)> = Vec::with_capacity(self.attachments.len());
        for attachment in &self.attachments {
            if !valid_attachment_name(&attachment.name)
                || attachment.name == "prompt.md"
                || decoded.iter().any(|(name, _)| *name == attachment.name)
            {
                return Err(HandoffError::AttachmentName(attachment.name.clone()));
            }
            let bytes = decode_base64(&attachment.base64)
                .ok_or_else(|| HandoffError::AttachmentEncoding(attachment.name.clone()))?;
            total += bytes.len();
            if total > ATTACHMENT_LIMIT {
                return Err(HandoffError::AttachmentSize);
            }
            decoded.push((attachment.name.clone(), bytes));
        }
        Ok(decoded)
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
pub fn encode_base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}
pub fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let value = |c: u8| -> Option<u32> {
        ALPHABET
            .iter()
            .position(|&a| a == c)
            .and_then(|p| u32::try_from(p).ok())
    };
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let chunks = bytes.len() / 4;
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index + 1 == chunks;
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = (n << 6) | value(c)?;
        }
        n <<= 6 * pad as u32;
        let decoded = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&decoded[..3 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handoff() -> SessionHandoff {
        SessionHandoff {
            extra: Default::default(),
            operation_id: OperationId::parse("handoff-1").unwrap(),
            account: "work".into(),
            model: Some("claude-opus-4-1[1m]".into()),
            prompt: "# Session hand-off\n\nContinue the chair.".into(),
            branches: vec!["feat/x".into()],
            attachments: vec![Attachment {
                extra: Default::default(),
                name: "notes.md".into(),
                base64: encode_base64(b"hello"),
            }],
            launch: true,
        }
    }

    #[test]
    fn base64_round_trips_every_padding_length() {
        for bytes in [&b""[..], b"a", b"ab", b"abc", b"abcd", b"abcde", b"abcdef"] {
            assert_eq!(decode_base64(&encode_base64(bytes)).unwrap(), bytes);
        }
        assert_eq!(encode_base64(b"abcdef"), "YWJjZGVm");
        assert_eq!(encode_base64(b"ab"), "YWI=");
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(decode_base64(&encode_base64(&all)).unwrap(), all);
        for bad in ["YWJ", "YW=j", "Y===", "YWI=YWI=", "YWJ*"] {
            assert!(decode_base64(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn validation_bounds_names_branches_and_attachment_bytes() {
        assert_eq!(handoff().validate().unwrap()[0].1, b"hello");
        let refuse = |edit: &dyn Fn(&mut SessionHandoff), expected: HandoffError| {
            let mut value = handoff();
            edit(&mut value);
            assert_eq!(value.validate().unwrap_err(), expected);
        };
        refuse(&|h| h.prompt = " ".into(), HandoffError::Prompt);
        // A prompt that could parse as a `claude` option never reaches the peer.
        refuse(
            &|h| h.prompt = "--allowedTools Bash".into(),
            HandoffError::Prompt,
        );
        refuse(
            &|h| h.prompt = format!("{PROMPT_HEADER}{}", "x".repeat(PROMPT_LIMIT)),
            HandoffError::Prompt,
        );
        refuse(&|h| h.account = "../x".into(), HandoffError::Account);
        refuse(&|h| h.model = Some("a b".into()), HandoffError::Model);
        for branch in ["../x", "x/", "-x", "a..b", "x.lock", "HEAD", "a b"] {
            refuse(
                &|h| h.branches = vec![branch.into()],
                HandoffError::Branches,
            );
        }
        refuse(
            &|h| h.branches = vec!["a".into(), "a".into()],
            HandoffError::Branches,
        );
        for name in ["../x", "a/b", ".hidden", "prompt.md", ""] {
            refuse(
                &|h| h.attachments[0].name = name.into(),
                HandoffError::AttachmentName(name.into()),
            );
        }
        refuse(
            &|h| h.attachments[0].base64 = "!!!!".into(),
            HandoffError::AttachmentEncoding("notes.md".into()),
        );
        refuse(
            &|h| {
                h.attachments = vec![
                    Attachment {
                        extra: Default::default(),
                        name: "a".into(),
                        base64: encode_base64(&vec![0; ATTACHMENT_LIMIT]),
                    },
                    Attachment {
                        extra: Default::default(),
                        name: "b".into(),
                        base64: encode_base64(b"x"),
                    },
                ]
            },
            HandoffError::AttachmentSize,
        );
    }
}
