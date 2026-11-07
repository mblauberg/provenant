//! Host-local opt-in repositories for the scheduled proposal sweep.
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub repositories: BTreeMap<String, Repository>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub directory: PathBuf,
    pub cadence_seconds: u64,
}

impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors = Vec::new();
        for (name, repository) in &self.repositories {
            for (field, invalid) in [
                ("directory", !repository.directory.is_absolute()),
                (
                    "cadence_seconds",
                    repository.cadence_seconds == 0
                        || repository.cadence_seconds > i64::MAX as u64 / 1_000_000,
                ),
            ] {
                if invalid {
                    errors.push(SectionError {
                        field: format!("repositories.{name}.{field}"),
                        code: "range",
                        detail: None,
                    });
                }
            }
        }
        errors
    }
}

pub const SECTION: DomainSection =
    DomainSection::new::<Settings>("steward", |_| "[repositories]\n")
        .with_boundary_checks()
        .with_fields(&[
            FieldMetadata::new("repositories.{repository}.directory", "string"),
            FieldMetadata::int(
                "repositories.{repository}.cadence_seconds",
                1,
                i64::MAX / 1_000_000,
            ),
        ]);
