use serde::Deserialize;

#[derive(Clone, Default, Deserialize)]
pub(crate) struct Raw {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) cache_read_input_tokens: Option<u64>,
    pub(crate) cache_creation_input_tokens: Option<u64>,
    pub(crate) cache_creation: Option<Split>,
    pub(crate) output_tokens: Option<u64>,
}
#[derive(Clone, Default, Deserialize)]
pub(crate) struct Split {
    pub(crate) ephemeral_1h_input_tokens: Option<u64>,
}
impl Raw {
    pub(crate) fn counts(&self) -> [Option<u64>; 4] {
        let one_hour = self
            .cache_creation
            .as_ref()
            .and_then(|split| split.ephemeral_1h_input_tokens);
        [
            self.input_tokens,
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens.max(one_hour),
            self.output_tokens,
        ]
    }

    /// A message repeats as it streams; the largest count of each field is its
    /// final one.
    pub(crate) fn merge(&mut self, other: Self) {
        let max = |a: Option<u64>, b: Option<u64>| a.max(b);
        self.input_tokens = max(self.input_tokens, other.input_tokens);
        self.cache_read_input_tokens =
            max(self.cache_read_input_tokens, other.cache_read_input_tokens);
        self.cache_creation_input_tokens = max(
            self.cache_creation_input_tokens,
            other.cache_creation_input_tokens,
        );
        self.output_tokens = max(self.output_tokens, other.output_tokens);
        let one_hour = |raw: &Self| raw.cache_creation.as_ref()?.ephemeral_1h_input_tokens;
        let merged = max(one_hour(self), one_hour(&other));
        self.cache_creation = merged.map(|n| Split {
            ephemeral_1h_input_tokens: Some(n),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merges_streamed_one_hour_usage() {
        let mut raw: Raw =
            serde_json::from_str(r#"{"cache_creation":{"ephemeral_1h_input_tokens":7}}"#).unwrap();
        let later: Raw = serde_json::from_str(
            r#"{"cache_creation_input_tokens":3,"cache_creation":{"ephemeral_1h_input_tokens":5}}"#,
        )
        .unwrap();
        raw.merge(later);
        assert_eq!(raw.counts()[2], Some(7));
    }
}
