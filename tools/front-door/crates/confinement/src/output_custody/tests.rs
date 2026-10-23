use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Seek, SeekFrom};
use std::path::Path;
use tempfile::tempdir;

const SOURCE_BYTES: &[u8] = b"hello\0world\xff";

fn run_cli_at_width<I, T>(args: I, columns: usize) -> CommandOutput
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    match parse_request_with_width(args, columns) {
        Ok(request) => execute(&LocalFs, request),
        Err(output) => output,
    }
}

fn run_cli_fixed<I, T>(args: I) -> CommandOutput
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    run_cli_at_width(args, 80)
}

fn prepared_root() -> tempfile::TempDir {
    let root = tempdir().unwrap();
    fs::write(root.path().join("source"), SOURCE_BYTES).unwrap();
    root
}

#[test]
fn lexical_destination_policy_preserves_python_path_checks() {
    for path in ["", "/", "a/", "a/.", "a/..", "a/../b"] {
        assert!(destination_parts(Path::new(path)).is_err(), "{path}");
    }
    let (absolute, parts, leaf) = destination_parts(Path::new("/a//./b")).unwrap();
    assert!(absolute);
    assert_eq!(parts, vec![OsString::from("a")]);
    assert_eq!(leaf, "b");
}

#[test]
fn digest_starts_at_zero_and_covers_binary_chunks() {
    use sha2::{Digest, Sha256};
    let mut data = io::Cursor::new(vec![255; 2 * 1024 * 1024 + 1]);
    data.set_position(900);
    let expected = digest_string(&Sha256::digest(data.get_ref()));
    assert_eq!(hash_file(&mut data).unwrap(), expected);
}

#[cfg(unix)]
#[test]
fn local_fs_replaces_symlinks_atomically_and_keeps_the_target() {
    let root = prepared_root();
    let target = root.path().join("target");
    let destination = root.path().join("result");
    fs::write(&target, b"preserve target").unwrap();
    std::os::unix::fs::symlink(&target, &destination).unwrap();

    let output = install(
        &LocalFs,
        Source::Path(&root.path().join("source")),
        &destination,
    )
    .unwrap();

    assert_eq!(fs::read(&destination).unwrap(), SOURCE_BYTES);
    assert_eq!(fs::read(&target).unwrap(), b"preserve target");
    assert_eq!(output.digest, digest_string(&Sha256::digest(SOURCE_BYTES)));
    verify(&LocalFs, &destination, &output).unwrap();
}

#[cfg(unix)]
#[test]
fn local_fs_handles_dangling_links_and_refuses_directories() {
    let root = prepared_root();
    let source = root.path().join("source");
    let dangling = root.path().join("dangling");
    std::os::unix::fs::symlink(root.path().join("absent"), &dangling).unwrap();
    let output = install(&LocalFs, Source::Path(&source), &dangling).unwrap();
    verify(&LocalFs, &dangling, &output).unwrap();

    let dir = root.path().join("directory");
    fs::create_dir(&dir).unwrap();
    assert!(matches!(
        install(&LocalFs, Source::Path(&source), &dir),
        Err(CustodyError::Refused("output destination is a directory"))
    ));
    let link_to_dir = root.path().join("link-to-dir");
    std::os::unix::fs::symlink(&dir, &link_to_dir).unwrap();
    assert!(matches!(
        install(&LocalFs, Source::Path(&source), &link_to_dir),
        Err(CustodyError::Refused(
            "output destination symlink does not target a regular file"
        ))
    ));
}

#[cfg(unix)]
#[test]
fn local_fs_refuses_untrusted_parent_links_and_special_sources() {
    let root = prepared_root();
    let parent = root.path().join("real-parent");
    fs::create_dir(&parent).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&parent, &alias).unwrap();
    assert!(matches!(
        install(
            &LocalFs,
            Source::Path(&root.path().join("source")),
            &alias.join("result")
        ),
        Err(CustodyError::Refused(
            "output destination parent is a symlink"
        ))
    ));
    assert!(matches!(
        install(
            &LocalFs,
            Source::Path(&parent),
            &root.path().join("bad-source")
        ),
        Err(CustodyError::Refused(
            "dispatch output source is not a regular file"
        ))
    ));
}

#[cfg(unix)]
#[test]
fn source_handle_is_read_from_start_and_verification_binds_inode_and_links() {
    let root = prepared_root();
    let source_path = root.path().join("source");
    let mut source = LocalFs.open_source(&source_path).unwrap();
    source.seek(SeekFrom::End(0)).unwrap();
    let destination = root.path().join("result");
    let output = install(&LocalFs, Source::Handle(&*source), &destination).unwrap();
    assert_eq!(fs::read(&destination).unwrap(), SOURCE_BYTES);
    verify(&LocalFs, &destination, &output).unwrap();

    let linked = root.path().join("hard-link");
    fs::hard_link(&destination, &linked).unwrap();
    assert!(verify(&LocalFs, &destination, &output).is_err());
    fs::remove_file(linked).unwrap();
    let replacement = root.path().join("replacement");
    fs::write(&replacement, SOURCE_BYTES).unwrap();
    fs::rename(&replacement, &destination).unwrap();
    assert!(verify(&LocalFs, &destination, &output).is_err());
}

#[cfg(unix)]
#[test]
fn local_fs_rejects_mutated_bytes_and_compares_temp_paths_canonically() {
    let root = prepared_root();
    let lexical_root = root.path().to_path_buf();
    let canonical_root = fs::canonicalize(&lexical_root).unwrap();
    assert_eq!(fs::canonicalize(&lexical_root).unwrap(), canonical_root);
    let source = lexical_root.join("source");
    let destination = lexical_root.join("result");
    let output = install(&LocalFs, Source::Path(&source), &destination).unwrap();
    assert_eq!(
        fs::canonicalize(destination.parent().unwrap()).unwrap(),
        canonical_root
    );
    fs::write(&destination, b"changed").unwrap();
    assert!(verify(&LocalFs, &destination, &output).is_err());
}

#[test]
fn cli_parser_and_renderer_keep_install_and_verify_wire_shapes() {
    let root = prepared_root();
    let source = root.path().join("source");
    let destination = root.path().join("result");
    let args = [
        "output_custody",
        "install",
        "--source",
        source.to_str().unwrap(),
        "--destination",
        destination.to_str().unwrap(),
    ];
    let output = run_cli_fixed(args);
    assert_eq!(output.exit_code, 0);
    assert!(output.stderr.is_empty());
    let fields: Vec<_> = output.stdout.split_whitespace().collect();
    assert_eq!(fields.len(), 3);
    assert!(fields[0].starts_with("sha256:"));
    assert_eq!(fs::read(destination).unwrap(), SOURCE_BYTES);

    let help = run_cli_fixed(["output_custody", "--help"]);
    assert_eq!(help.exit_code, 0);
    assert!(help
        .stdout
        .contains("Install and verify one identity-bound dispatch output."));
    assert!(help.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn publication_and_failure_edges_match_the_python_implementation() {
    let py_root = prepared_root();
    let py_target = py_root.path().join("target");
    let py_destination = py_root.path().join("result");
    let py_directory = py_root.path().join("destination-directory");
    let py_bad_source = py_root.path().join("source-directory");
    fs::write(&py_target, b"keep").unwrap();
    fs::create_dir(&py_directory).unwrap();
    fs::create_dir(&py_bad_source).unwrap();
    std::os::unix::fs::symlink(&py_target, &py_destination).unwrap();
    let fixture = serde_json::json!({
        "source": py_root.path().join("source"),
        "destination": py_destination,
        "target": py_target,
        "directory": py_directory,
        "bad_source": py_bad_source,
    });
    let Some(expected) = crate::python::oracle(
        r#"
import output_custody
from pathlib import Path
source=fixture['source']; destination=fixture['destination']
digest, device, inode=output_custody.install(source, destination)
output_custody.verify(destination, digest, device, inode)
data=Path(destination).read_bytes()
target=Path(fixture['target']).read_bytes()
Path(destination).write_bytes(b'changed')
try:
    output_custody.verify(destination, digest, device, inode)
    changed_refused=False
except (output_custody.CustodyError, OSError):
    changed_refused=True
try:
    output_custody.install(source, fixture['directory'])
    directory_refused=False
except (output_custody.CustodyError, OSError):
    directory_refused=True
try:
    output_custody.install(fixture['bad_source'], str(fixture['destination']) + '.bad')
    source_refused=False
except (output_custody.CustodyError, OSError):
    source_refused=True
print(json.dumps({'digest': digest, 'bytes': list(data), 'target': list(target),
                  'changed_refused': changed_refused,
                  'directory_refused': directory_refused,
                  'source_refused': source_refused}))
"#,
        &fixture,
    ) else {
        return;
    };

    let rust_root = prepared_root();
    let rust_target = rust_root.path().join("target");
    let rust_destination = rust_root.path().join("result");
    let rust_directory = rust_root.path().join("destination-directory");
    let rust_bad_source = rust_root.path().join("source-directory");
    fs::write(&rust_target, b"keep").unwrap();
    fs::create_dir(&rust_directory).unwrap();
    fs::create_dir(&rust_bad_source).unwrap();
    std::os::unix::fs::symlink(&rust_target, &rust_destination).unwrap();
    let output = install(
        &LocalFs,
        Source::Path(&rust_root.path().join("source")),
        &rust_destination,
    )
    .unwrap();
    verify(&LocalFs, &rust_destination, &output).unwrap();
    let bytes = fs::read(&rust_destination).unwrap();
    let target = fs::read(&rust_target).unwrap();
    fs::write(&rust_destination, b"changed").unwrap();
    let actual = serde_json::json!({
        "digest": output.digest,
        "bytes": bytes,
        "target": target,
        "changed_refused": verify(&LocalFs, &rust_destination, &output).is_err(),
        "directory_refused": install(
            &LocalFs,
            Source::Path(&rust_root.path().join("source")),
            &rust_directory
        ).is_err(),
        "source_refused": install(
            &LocalFs,
            Source::Path(&rust_bad_source),
            &rust_root.path().join("result.bad")
        ).is_err(),
    });
    assert_eq!(actual, expected);
}

#[test]
fn python_integer_matches_decimal_int_acceptance_and_identity_bounds() {
    let cases = [
        ("0", PythonInteger::InRange(0)),
        ("+1_024", PythonInteger::InRange(1024)),
        (" -0 ", PythonInteger::InRange(0)),
        ("\u{0661}\u{0662}", PythonInteger::InRange(12)),
        ("１_２", PythonInteger::InRange(12)),
        ("𝟡", PythonInteger::InRange(9)),
        ("\u{00a0}1\u{00a0}", PythonInteger::InRange(1)),
        ("-1", PythonInteger::NumberOutOfRange),
        ("18446744073709551616", PythonInteger::NumberOutOfRange),
    ];
    for (text, expected) in cases {
        assert_eq!(text.parse::<PythonInteger>().unwrap(), expected);
    }
    for invalid in ["", "_1", "1_", "1__2", "1.0", "0x10", "\u{001c}1\u{001c}"] {
        assert!(invalid.parse::<PythonInteger>().is_err(), "{invalid}");
    }
    assert!("9".repeat(5000).parse::<PythonInteger>().is_err());
    let parsed = parse_request([
        "output_custody",
        "verify",
        "--destination",
        "result",
        "--digest",
        "sha256:00",
        "--device",
        "-1",
        "--inode",
        "0",
    ])
    .unwrap();
    assert!(matches!(
        parsed,
        Request::Verify {
            device: PythonInteger::NumberOutOfRange,
            ..
        }
    ));

    let oversized = "9".repeat(5000);
    let mut values: Vec<_> = cases.iter().map(|(text, _)| *text).collect();
    values.push(&oversized);
    let Some(expected) = crate::python::oracle(
        r#"
def classify(value):
    try:
        number=int(value)
    except ValueError:
        return 'invalid'
    return 'in-range' if 0 <= number <= 18446744073709551615 else 'other'
print(json.dumps([classify(value) for value in fixture['values']]))
"#,
        &serde_json::json!({"values": values}),
    ) else {
        return;
    };
    let actual: Vec<_> = values
        .iter()
        .map(|value| match value.parse::<PythonInteger>() {
            Ok(PythonInteger::InRange(_)) => "in-range",
            Ok(PythonInteger::NumberOutOfRange) => "other",
            Err(_) => "invalid",
        })
        .collect();
    assert_eq!(serde_json::json!(actual), expected);
}

#[cfg(unix)]
#[test]
fn cli_io_diagnostic_matches_python_for_a_missing_output() {
    let py_root = tempdir().unwrap();
    let py_destination = py_root.path().join("missing-output");
    let Some(expected) = crate::python::oracle(
        r#"
import contextlib, io, output_custody
stderr=io.StringIO()
with contextlib.redirect_stderr(stderr):
    code=output_custody.main(['verify', '--destination', fixture['destination'],
                              '--digest', 'sha256:00', '--device', '0', '--inode', '0'])
print(json.dumps({'exit_code': code, 'stdout': '', 'stderr': stderr.getvalue()}))
"#,
        &serde_json::json!({"destination": py_destination}),
    ) else {
        return;
    };

    let rust_root = tempdir().unwrap();
    let rust_destination = rust_root.path().join("missing-output");
    let actual = run_cli_fixed([
        "output_custody".to_owned(),
        "verify".to_owned(),
        "--destination".to_owned(),
        rust_destination.to_string_lossy().into_owned(),
        "--digest".to_owned(),
        "sha256:00".to_owned(),
        "--device".to_owned(),
        "0".to_owned(),
        "--inode".to_owned(),
        "0".to_owned(),
    ]);
    assert_eq!(
        serde_json::json!({
            "exit_code": actual.exit_code,
            "stdout": actual.stdout,
            "stderr": actual.stderr,
        }),
        expected
    );
}

#[cfg(unix)]
#[test]
fn duplicated_source_handle_keeps_python_dup_shared_offset_behavior() {
    let py_root = prepared_root();
    let py_destination = py_root.path().join("result");
    let Some(expected) = crate::python::oracle(
        r#"
import os, output_custody
fd=os.open(fixture['source'], os.O_RDONLY)
os.lseek(fd, 3, os.SEEK_SET)
output_custody.install(fd, fixture['destination'])
position=os.lseek(fd, 0, os.SEEK_CUR)
with open(fixture['destination'], 'rb') as output:
    data=output.read()
print(json.dumps({'bytes': list(data), 'position': position}))
os.close(fd)
"#,
        &serde_json::json!({"source": py_root.path().join("source"), "destination": py_destination}),
    ) else {
        return;
    };

    let root = prepared_root();
    let mut original = LocalFs.open_source(&root.path().join("source")).unwrap();
    original.seek(SeekFrom::Start(3)).unwrap();
    let destination = root.path().join("result");
    install(&LocalFs, Source::Handle(&*original), &destination).unwrap();
    let bytes = fs::read(&destination).unwrap();
    let mut remaining = Vec::new();
    original.read_to_end(&mut remaining).unwrap();
    assert_eq!(
        serde_json::json!({"bytes": bytes, "position": SOURCE_BYTES.len()}),
        expected
    );
    assert!(
        remaining.is_empty(),
        "dup shares the open-file-description offset"
    );
}

#[cfg(unix)]
#[test]
fn cli_io_diagnostic_uses_python_surrogateescape_repr_for_path_bytes() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let py_root = tempdir().unwrap();
    let py_destination = py_root
        .path()
        .join(OsString::from_vec(b"missing-\'quote-\xff".to_vec()));
    let Some(expected) = crate::python::oracle(
        r#"
import contextlib, io, os, output_custody
destination=os.fsdecode(bytes(fixture['destination_bytes']))
stderr=io.StringIO()
with contextlib.redirect_stderr(stderr):
    code=output_custody.main(['verify', '--destination', destination,
                              '--digest', 'sha256:00', '--device', '0', '--inode', '0'])
print(json.dumps({'exit_code': code, 'stdout': '', 'stderr': stderr.getvalue()}))
"#,
        &serde_json::json!({"destination_bytes": py_destination.as_os_str().as_bytes()}),
    ) else {
        return;
    };

    let rust_root = tempdir().unwrap();
    let rust_destination = rust_root
        .path()
        .join(OsString::from_vec(b"missing-\'quote-\xff".to_vec()));
    let output = run_cli_fixed(vec![
        OsString::from("output_custody"),
        OsString::from("verify"),
        OsString::from("--destination"),
        rust_destination.into_os_string(),
        OsString::from("--digest"),
        OsString::from("sha256:00"),
        OsString::from("--device"),
        OsString::from("0"),
        OsString::from("--inode"),
        OsString::from("0"),
    ]);
    assert_eq!(
        serde_json::json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
        }),
        expected
    );
}

#[cfg(unix)]
#[test]
fn local_fs_accepts_the_root_owned_darwin_var_alias() {
    let var = Path::new("/var");
    if fs::symlink_metadata(var)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
        && fs::canonicalize(var).ok().as_deref() == Some(Path::new("/private/var"))
    {
        let alias_directory = Path::new("/var/tmp");
        assert_eq!(
            fs::canonicalize(alias_directory).unwrap(),
            Path::new("/private/var/tmp")
        );
        let (directory, leaf) = open_parent(
            &LocalFs,
            &alias_directory.join("provenant-port-alias-probe"),
        )
        .unwrap();
        assert_eq!(leaf, "provenant-port-alias-probe");
        assert_eq!(directory.metadata().unwrap().kind, FileKind::Directory);
    }
}

#[test]
fn cli_missing_command_diagnostic_matches_python_parser() {
    let Some(expected) = crate::python::oracle(
        r#"
import argparse, contextlib, io, output_custody, sys
sys.argv[0]='output_custody.py'
stderr=io.StringIO()
try:
    with contextlib.redirect_stderr(stderr):
        output_custody.main([])
except SystemExit as error:
    code=error.code
print(json.dumps({'exit_code': code, 'stdout': '', 'stderr': stderr.getvalue()}))
"#,
        &serde_json::json!({}),
    ) else {
        return;
    };
    let actual = run_cli_fixed(["output_custody.py"]);
    assert_eq!(
        serde_json::json!({
            "exit_code": actual.exit_code,
            "stdout": actual.stdout,
            "stderr": actual.stderr,
        }),
        expected
    );
}

#[test]
fn cli_parser_diagnostics_match_python_argparse() {
    let cases = vec![
        serde_json::json!([]),
        serde_json::json!(["--help"]),
        serde_json::json!(["--", "--help"]),
        serde_json::json!(["--", "-h"]),
        serde_json::json!(["--", "--unknown"]),
        serde_json::json!(["install", "--help"]),
        serde_json::json!(["--help=ignored"]),
        serde_json::json!(["--he"]),
        serde_json::json!(["install", "--help=ignored"]),
        serde_json::json!(["verify", "--d", "out"]),
        serde_json::json!(["frobnicate"]),
        serde_json::json!(["--unknown"]),
        serde_json::json!(["--unknown", "install"]),
        serde_json::json!(["--unknown", "frobnicate"]),
        serde_json::json!([
            "--unknown",
            "install",
            "--source",
            "source",
            "--destination",
            "destination"
        ]),
        serde_json::json!(["install", "--unknown"]),
        serde_json::json!(["install", "--source", "one", "--unknown", "value"]),
        serde_json::json!(["install", "--source", "one"]),
        serde_json::json!([
            "verify",
            "--destination",
            "out",
            "--digest",
            "sha256:00",
            "--device",
            "nope",
            "--inode",
            "0"
        ]),
        serde_json::json!(["verify", "--device", "-"]),
        serde_json::json!(["verify", "--device", "-1."]),
        serde_json::json!(["verify", "--device", "-1x"]),
        serde_json::json!(["verify", "--device", "-.5x"]),
        serde_json::json!(["verify", "--device", "-1.5"]),
        serde_json::json!(["verify", "--device", "-١"]),
        serde_json::json!(["verify", "--device", "-٠"]),
        serde_json::json!(["-hfoo"]),
        serde_json::json!(["-hh"]),
        serde_json::json!(["install", "-hfoo"]),
        serde_json::json!(["verify", "-hh"]),
        serde_json::json!(["-h=ignored"]),
        serde_json::json!(["install", "-h=ignored"]),
        serde_json::json!(["install", "--source", "-"]),
        serde_json::json!(["--", "install", "--source", "s", "--destination", "d"]),
        serde_json::json!(["install", "--", "--source", "s", "--destination", "d"]),
        serde_json::json!(["install", "--source", "s", "--", "--destination", "d"]),
        serde_json::json!([
            "verify",
            "--destination",
            "out",
            "--digest",
            "sha256:00",
            "--device",
            "0",
            "--inode",
            "0",
            "extra"
        ]),
        serde_json::json!(["install", "--source"]),
    ];
    let Some(expected) = crate::python::oracle(
        r#"
import contextlib, io, os, output_custody, sys
os.environ['COLUMNS']='80'
sys.argv[0]=fixture['argv0']
results=[]
for arguments in fixture['cases']:
    stdout=io.StringIO(); stderr=io.StringIO()
    try:
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code=output_custody.main(arguments)
    except SystemExit as error:
        code=error.code
    results.append({'exit_code': code, 'stdout': stdout.getvalue(), 'stderr': stderr.getvalue()})
print(json.dumps(results))
"#,
        &serde_json::json!({"argv0": "output_custody.py", "cases": cases}),
    ) else {
        return;
    };
    let actual: Vec<_> = cases
        .iter()
        .map(|arguments| {
            let mut argv = vec![OsString::from("output_custody.py")];
            argv.extend(
                arguments
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| OsString::from(value.as_str().unwrap())),
            );
            let output = run_cli_fixed(argv);
            serde_json::json!({
                "exit_code": output.exit_code,
                "stdout": output.stdout,
                "stderr": output.stderr,
            })
        })
        .collect();
    for (index, actual) in actual.iter().enumerate() {
        assert_eq!(actual, &expected[index], "parser fixture: {}", cases[index]);
    }
}

#[test]
fn help_usage_matches_argparse_widths_and_program_name_lengths() {
    let programs = [
        "x",
        "output_custody.py",
        "mid-executable-name-with-forty-five-chars-123",
        "executable-name-longer-than-the-usage-heading-threshold-to-wrap-cleanly",
    ];
    let columns = [40, 60, 100];
    let cases = [
        vec!["--help"],
        vec!["install", "--help"],
        vec!["verify", "--help"],
        vec!["unknown"],
        vec!["install", "--source", "source"],
    ];
    let Some(expected) = crate::python::oracle(
        r#"
import contextlib, io, os, output_custody, sys
results=[]
for columns in fixture['columns']:
    os.environ['COLUMNS']=str(columns)
    width_results=[]
    for program in fixture['programs']:
        sys.argv[0]=program
        program_results=[]
        for arguments in fixture['cases']:
            stdout=io.StringIO(); stderr=io.StringIO()
            try:
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    code=output_custody.main(arguments)
            except SystemExit as error:
                code=error.code
            program_results.append({'exit_code': code, 'stdout': stdout.getvalue(), 'stderr': stderr.getvalue()})
        width_results.append(program_results)
    results.append(width_results)
print(json.dumps(results))
"#,
        &serde_json::json!({"columns": columns, "programs": programs, "cases": cases}),
    ) else {
        return;
    };
    let actual: Vec<_> = columns
        .iter()
        .map(|columns| {
            programs
                .iter()
                .map(|program| {
                    cases
                        .iter()
                        .map(|arguments| {
                            let mut argv = vec![OsString::from(program)];
                            argv.extend(arguments.iter().map(OsString::from));
                            let output = run_cli_at_width(argv, *columns);
                            serde_json::json!({
                                "exit_code": output.exit_code,
                                "stdout": output.stdout,
                                "stderr": output.stderr,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .collect();
    for (width_index, (actual_width, expected_width)) in
        actual.iter().zip(expected.as_array().unwrap()).enumerate()
    {
        for (program_index, (actual_program, expected_program)) in actual_width
            .iter()
            .zip(expected_width.as_array().unwrap())
            .enumerate()
        {
            for (case_index, (actual_case, expected_case)) in actual_program
                .iter()
                .zip(expected_program.as_array().unwrap())
                .enumerate()
            {
                assert_eq!(
                    actual_case,
                    expected_case,
                    "width {}, program {}, case {:?}\nactual: {}\nexpected: {}",
                    columns[width_index],
                    programs[program_index],
                    cases[case_index],
                    actual_case,
                    expected_case
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn fifo_source_refusal_matches_python_when_nonblocking_peer_is_open() {
    let root = tempdir().unwrap();
    let source = root.path().join("source-fifo");
    let destination = root.path().join("result");
    let fixture = serde_json::json!({
        "source": source.to_str().unwrap(),
        "destination": destination.to_str().unwrap(),
    });
    let Some(expected) = crate::python::oracle(
        r#"
import contextlib, io, os, output_custody
os.mkfifo(fixture['source'])
peer=os.open(fixture['source'], os.O_RDWR | os.O_NONBLOCK)
stderr=io.StringIO()
try:
    with contextlib.redirect_stderr(stderr):
        code=output_custody.main(['install', '--source', fixture['source'],
                                  '--destination', fixture['destination']])
finally:
    os.close(peer)
print(json.dumps({'exit_code': code, 'stdout': '', 'stderr': stderr.getvalue()}))
"#,
        &fixture,
    ) else {
        return;
    };
    let actual = run_cli_fixed([
        "output_custody".to_owned(),
        "install".to_owned(),
        "--source".to_owned(),
        source.to_string_lossy().into_owned(),
        "--destination".to_owned(),
        destination.to_string_lossy().into_owned(),
    ]);
    assert_eq!(
        serde_json::json!({
            "exit_code": actual.exit_code,
            "stdout": actual.stdout,
            "stderr": actual.stderr,
        }),
        expected
    );
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InjectedFailure {
    None,
    FileSync,
    Replace,
    DirectorySync,
    InstalledOpen,
    VerifyReopen,
}

#[cfg(unix)]
struct SharedCell<T>(std::sync::Mutex<T>);
#[cfg(unix)]
impl<T: Copy> SharedCell<T> {
    fn new(value: T) -> Self {
        Self(std::sync::Mutex::new(value))
    }
    fn get(&self) -> T {
        *self.0.lock().unwrap()
    }
    fn set(&self, value: T) {
        *self.0.lock().unwrap() = value;
    }
}

#[cfg(unix)]
struct InjectedControls {
    failure: SharedCell<InjectedFailure>,
    open_reads: SharedCell<usize>,
    temporary_names: SharedCell<usize>,
}

#[cfg(unix)]
struct InjectedFs {
    inner: LocalFs,
    controls: std::sync::Arc<InjectedControls>,
    fixed_name: Option<OsString>,
}

#[cfg(unix)]
impl InjectedFs {
    fn new(failure: InjectedFailure) -> Self {
        Self {
            inner: LocalFs,
            controls: std::sync::Arc::new(InjectedControls {
                failure: SharedCell::new(failure),
                open_reads: SharedCell::new(0),
                temporary_names: SharedCell::new(0),
            }),
            fixed_name: None,
        }
    }

    fn with_fixed_name(name: OsString) -> Self {
        let mut fs = Self::new(InjectedFailure::None);
        fs.fixed_name = Some(name);
        fs
    }
}

#[cfg(unix)]
impl SafeFs for InjectedFs {
    fn root(&self, absolute: bool) -> io::Result<Box<dyn SafeDir>> {
        Ok(Box::new(InjectedDir {
            inner: self.inner.root(absolute)?,
            controls: self.controls.clone(),
        }))
    }

    fn open_anchor(&self, path: &Path) -> io::Result<Box<dyn SafeDir>> {
        Ok(Box::new(InjectedDir {
            inner: self.inner.open_anchor(path)?,
            controls: self.controls.clone(),
        }))
    }

    fn open_source(&self, path: &Path) -> io::Result<Box<dyn SafeFile>> {
        self.inner.open_source(path)
    }

    fn temporary_name(&self) -> io::Result<OsString> {
        self.controls
            .temporary_names
            .set(self.controls.temporary_names.get() + 1);
        match &self.fixed_name {
            Some(name) => Ok(name.clone()),
            None => self.inner.temporary_name(),
        }
    }
}

#[cfg(unix)]
struct InjectedDir {
    inner: Box<dyn SafeDir>,
    controls: std::sync::Arc<InjectedControls>,
}

#[cfg(unix)]
impl SafeDir for InjectedDir {
    fn metadata(&self) -> io::Result<Metadata> {
        self.inner.metadata()
    }

    fn metadata_at(&self, name: &OsStr, follow: bool) -> io::Result<Metadata> {
        self.inner.metadata_at(name, follow)
    }

    fn open_directory(&self, name: &OsStr, follow: bool) -> io::Result<Box<dyn SafeDir>> {
        Ok(Box::new(Self {
            inner: self.inner.open_directory(name, follow)?,
            controls: self.controls.clone(),
        }))
    }

    fn open_verified_directory(&self, name: &OsStr) -> io::Result<Box<dyn SafeDir>> {
        Ok(Box::new(Self {
            inner: self.inner.open_verified_directory(name)?,
            controls: self.controls.clone(),
        }))
    }

    fn open_read(&self, name: &OsStr) -> io::Result<Box<dyn SafeFile>> {
        let count = self.controls.open_reads.get() + 1;
        self.controls.open_reads.set(count);
        let failure = self.controls.failure.get();
        if (failure == InjectedFailure::InstalledOpen && count == 1)
            || (failure == InjectedFailure::VerifyReopen && count == 2)
        {
            return Err(io::Error::from_raw_os_error(2));
        }
        self.inner.open_read(name)
    }

    fn create_new(&self, name: &OsStr) -> io::Result<Box<dyn SafeFile>> {
        Ok(Box::new(InjectedFile {
            inner: self.inner.create_new(name)?,
            controls: self.controls.clone(),
        }))
    }

    fn mkdir(&self, name: &OsStr, mode: u32) -> io::Result<()> {
        self.inner.mkdir(name, mode)
    }

    fn entries(&self) -> io::Result<Box<dyn Iterator<Item = io::Result<OsString>>>> {
        self.inner.entries()
    }

    fn make_writable(&self) -> io::Result<()> {
        self.inner.make_writable()
    }

    fn unlink(&self, name: &OsStr) -> io::Result<()> {
        self.inner.unlink(name)
    }

    fn rmdir(&self, name: &OsStr) -> io::Result<()> {
        self.inner.rmdir(name)
    }

    fn replace(&self, source: &OsStr, destination: &OsStr) -> io::Result<()> {
        if self.controls.failure.get() == InjectedFailure::Replace {
            return Err(io::Error::from_raw_os_error(1));
        }
        self.inner.replace(source, destination)
    }

    fn rename_noreplace(&self, source: &OsStr, destination: &OsStr) -> io::Result<()> {
        self.inner.rename_noreplace(source, destination)
    }

    fn exchange(&self, source: &OsStr, destination: &OsStr) -> io::Result<()> {
        self.inner.exchange(source, destination)
    }

    fn symlink(&self, target: &Path, name: &OsStr) -> io::Result<()> {
        self.inner.symlink(target, name)
    }

    fn sync(&self) -> io::Result<()> {
        if self.controls.failure.get() == InjectedFailure::DirectorySync {
            return Err(io::Error::from_raw_os_error(28));
        }
        self.inner.sync()
    }

    fn atomic_write(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
        mode: provenant_supervisor::fs::PublishMode,
    ) -> io::Result<()> {
        self.inner.atomic_write(name, prefix, bytes, mode)
    }

    fn replace_link(&self, name: &OsStr, target: &Path) -> io::Result<()> {
        self.inner.replace_link(name, target)
    }
}

#[cfg(unix)]
struct InjectedFile {
    inner: Box<dyn SafeFile>,
    controls: std::sync::Arc<InjectedControls>,
}

#[cfg(unix)]
impl Read for InjectedFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer)
    }
}

#[cfg(unix)]
impl Write for InjectedFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(unix)]
impl Seek for InjectedFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

#[cfg(unix)]
impl SafeFile for InjectedFile {
    fn metadata(&self) -> io::Result<Metadata> {
        self.inner.metadata()
    }

    fn duplicate(&self) -> io::Result<Box<dyn SafeFile>> {
        Ok(Box::new(Self {
            inner: self.inner.duplicate()?,
            controls: self.controls.clone(),
        }))
    }

    fn sync(&self) -> io::Result<()> {
        if self.controls.failure.get() == InjectedFailure::FileSync {
            return Err(io::Error::from_raw_os_error(28));
        }
        self.inner.sync()
    }

    fn try_lock(&self) -> io::Result<bool> {
        self.inner.try_lock()
    }

    fn unlock(&self) -> io::Result<()> {
        self.inner.unlock()
    }
}

#[cfg(unix)]
#[test]
fn install_retries_temporary_name_collisions_exactly_thirty_two_times() {
    let root = prepared_root();
    let source = root.path().join("source");
    let destination = root.path().join("result");
    let collision = OsString::from(".cf-dispatch-output.collision");
    fs::write(root.path().join(&collision), b"reserved").unwrap();
    fs::write(&destination, b"old output").unwrap();
    let os = InjectedFs::with_fixed_name(collision.clone());

    assert!(matches!(
        install(&os, Source::Path(&source), &destination),
        Err(CustodyError::Refused(
            "could not allocate an output temporary file"
        ))
    ));
    assert_eq!(os.controls.temporary_names.get(), 32);
    assert_eq!(fs::read(root.path().join(collision)).unwrap(), b"reserved");
    assert_eq!(fs::read(destination).unwrap(), b"old output");
}

#[cfg(unix)]
#[test]
fn install_failure_effects_match_each_publication_boundary() {
    for (failure, expected_output, temporary_remains) in [
        (InjectedFailure::FileSync, b"old output".as_slice(), false),
        (InjectedFailure::Replace, b"old output".as_slice(), false),
        (InjectedFailure::DirectorySync, SOURCE_BYTES, false),
        (InjectedFailure::InstalledOpen, SOURCE_BYTES, false),
    ] {
        let root = prepared_root();
        let source = root.path().join("source");
        let destination = root.path().join("result");
        fs::write(&destination, b"old output").unwrap();
        let os = InjectedFs::new(failure);
        assert!(install(&os, Source::Path(&source), &destination).is_err());
        assert_eq!(
            fs::read(&destination).unwrap(),
            expected_output,
            "{failure:?}"
        );
        let temporary_names = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".cf-dispatch-output.")
            })
            .count();
        assert_eq!(temporary_names > 0, temporary_remains, "{failure:?}");
    }
}

#[cfg(unix)]
#[test]
fn verify_reports_a_failed_final_reopen_after_hashing() {
    let root = prepared_root();
    let source = root.path().join("source");
    let destination = root.path().join("result");
    let output = install(&LocalFs, Source::Path(&source), &destination).unwrap();
    let os = InjectedFs::new(InjectedFailure::None);
    os.controls.open_reads.set(0);
    os.controls.failure.set(InjectedFailure::VerifyReopen);

    assert!(matches!(
        verify(&os, &destination, &output),
        Err(CustodyError::Io {
            operation: "reopen output",
            ..
        })
    ));
    assert_eq!(fs::read(destination).unwrap(), SOURCE_BYTES);
}

#[cfg(unix)]
#[test]
fn cli_parent_errors_follow_python_component_walk_order() {
    let py_root = prepared_root();
    let py_destination = py_root.path().join("missing").join("..").join("result");
    let Some(expected) = crate::python::oracle(
        r#"
import contextlib, io, output_custody
stderr=io.StringIO()
with contextlib.redirect_stderr(stderr):
    code=output_custody.main(['verify', '--destination', fixture['destination'],
                              '--digest', 'sha256:00', '--device', '0', '--inode', '0'])
print(json.dumps({'exit_code': code, 'stdout': '', 'stderr': stderr.getvalue()}))
"#,
        &serde_json::json!({"destination": py_destination}),
    ) else {
        return;
    };

    let rust_root = prepared_root();
    let rust_destination = rust_root.path().join("missing").join("..").join("result");
    let output = run_cli_fixed([
        OsString::from("output_custody.py"),
        OsString::from("verify"),
        OsString::from("--destination"),
        rust_destination.into_os_string(),
        OsString::from("--digest"),
        OsString::from("sha256:00"),
        OsString::from("--device"),
        OsString::from("0"),
        OsString::from("--inode"),
        OsString::from("0"),
    ]);
    assert_eq!(
        serde_json::json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
        }),
        expected
    );
}

#[cfg(unix)]
#[test]
fn raw_digest_is_preserved_until_verification() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempdir().unwrap();
    let path = root.path().join("output");
    std::fs::write(&path, b"content").unwrap();
    let Some(expected) = crate::python::oracle(
        r#"
import contextlib,io,os,output_custody
stderr=io.StringIO()
with contextlib.redirect_stderr(stderr):
 code=output_custody.main(['verify','--destination',fixture['path'],'--digest',os.fsdecode(b'\xff'),'--device','0','--inode','0'])
print(json.dumps({'exit_code':code,'stdout':'','stderr':stderr.getvalue()}))
"#,
        &serde_json::json!({"path":path}),
    ) else {
        return;
    };
    let actual = run_cli_fixed(vec![
        "output_custody".into(),
        "verify".into(),
        "--destination".into(),
        path.into_os_string(),
        "--digest".into(),
        OsString::from_vec(vec![255]),
        "--device".into(),
        "0".into(),
        "--inode".into(),
        "0".into(),
    ]);
    assert_eq!(
        serde_json::json!({"exit_code":actual.exit_code,"stdout":actual.stdout,"stderr":actual.stderr}),
        expected
    );
}
