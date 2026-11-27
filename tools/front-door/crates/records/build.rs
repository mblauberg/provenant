use std::io::Write as _;
use std::{collections::BTreeSet, error::Error, fmt::Write as _, fs, path::PathBuf};
use toml_edit::{DocumentMut, Table};

fn required<'a>(row: &'a Table, key: &str) -> Result<&'a str, Box<dyn Error>> {
    row.get(key)
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("policy rule missing string {key}").into())
}

fn optional(row: &Table, key: &str) -> Result<String, Box<dyn Error>> {
    match row.get(key) {
        None => Ok("None".to_owned()),
        Some(value) => Ok(format!(
            "Some({:?}.to_owned())",
            value
                .as_str()
                .ok_or_else(|| format!("policy {key} must be a string"))?
        )),
    }
}

// `cargo:` directives are a build script's protocol with Cargo, not command output.
#[allow(
    clippy::disallowed_methods,
    reason = "cargo: directives in a build script are the build protocol itself"
)]
fn emit_instruction(instruction: &str) -> std::io::Result<()> {
    writeln!(std::io::stdout().lock(), "cargo:{instruction}")
}

fn main() -> Result<(), Box<dyn Error>> {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest directory")?);
    let asset = manifest.join("../run/assets/policy.toml");
    emit_instruction(&format!("rerun-if-changed={}", asset.display()))?;
    let document = fs::read_to_string(asset)?.parse::<DocumentMut>()?;
    let rows = document["rule"]
        .as_array_of_tables()
        .ok_or("policy has no rules")?;
    let mut output = String::from("fn compiled_table() -> Vec<Rule> { vec![\n");
    let mut lookup =
        String::from("fn rule_warning_code(code: &str) -> Option<WarningCode> { match code {\n");
    let mut codes = BTreeSet::new();
    for row in rows {
        let code = required(row, "code")?;
        if !codes.insert(code.to_owned()) {
            return Err(format!("duplicate policy code {code}").into());
        }
        let boundary = match required(row, "boundary")? {
            "credentials" => "Credentials",
            "other_work" => "OtherWork",
            "authority" => "Authority",
            "operation" => "Operation",
            "none" => "None",
            value => return Err(format!("invalid policy boundary {value}").into()),
        };
        let strength = match required(row, "strength")? {
            "refuse" => "Refuse",
            "warn" => "Warn",
            "off" => "Off",
            value => return Err(format!("invalid policy strength {value}").into()),
        };
        let refusable = match row.get("refusable") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or("policy refusable must be a boolean")?,
        };
        writeln!(
            output,
            "Rule {{ code: {:?}.to_owned(), boundary: Boundary::{}, strength: Strength::{}, group: {}, knob: {}, justification: {:?}.to_owned(), site: {:?}.to_owned(), threshold: {}, refusable: {} }},",
            code,
            boundary,
            strength,
            optional(row, "group")?,
            optional(row, "knob")?,
            required(row, "justification")?,
            required(row, "site")?,
            optional(row, "threshold")?,
            refusable
        )?;
        writeln!(lookup, "{code:?} => Some(WarningCode({code:?})),")?;
    }
    output.push_str("] }\n");
    lookup.push_str("_ => None, } }\n");
    output.push_str(&lookup);
    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing output directory")?);
    fs::write(out.join("compiled_policy.rs"), output)?;
    Ok(())
}
