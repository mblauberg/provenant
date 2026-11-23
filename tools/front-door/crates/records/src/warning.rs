//! The typed warning shared by library and host code.
//!
//! A warning carries a stable code from the catalogue below and one sentence
//! for a person. The code, not the message, is the machine-readable identity;
//! the message never carries a secret value. Host boundaries turn a `Warning`
//! into the string warning list their receipt already reports.
use std::fmt;

/// One declared warning event: a stable code and a one-sentence message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub code: WarningCode,
    pub message: String,
}

impl Warning {
    pub const fn new(code: WarningCode, message: String) -> Self {
        Self { code, message }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// A declared warning code: one const per code with a one-line meaning.
/// Adding a code is additive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WarningCode(pub &'static str);

impl WarningCode {
    /// The stable UPPER_SNAKE spelling.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// A route model is a full `ADAPTER/ID` that the effective catalogue does not
/// know. The declared spelling is preserved; the warning names it.
pub const ROUTE_MODEL_NOT_CATALOGUED: WarningCode = WarningCode("ROUTE_MODEL_NOT_CATALOGUED");

/// The declared catalogue of warning codes. The test below keeps it canonical.
pub const WARNING_CODES: &[WarningCode] = &[ROUTE_MODEL_NOT_CATALOGUED];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_codes_form_one_declared_catalogue() {
        let mut seen = std::collections::BTreeSet::new();
        for code in WARNING_CODES {
            let name = code.as_str();
            assert!(
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte == b'_'),
                "{name}"
            );
            assert!(seen.insert(name), "duplicate warning code {name}");
        }
    }
}
