use super::*;
use std::path::Path;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn chair(account: &str, session: &str) -> ChairSession {
    ChairSession {
        adapter: "claude".into(),
        account: account.into(),
        session: session.into(),
        project: None,
        config: None,
        usage: Vec::new(),
        unread: None,
    }
}
fn assistant(id: &str, model: &str, usage: &str) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"id":"{id}","model":"{model}","content":[{{"type":"text","text":"never kept"}}],"usage":{usage}}}}}"#
    )
}
fn write(path: &Path, lines: &[String]) -> Result {
    std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    std::fs::write(path, lines.join("\n") + "\n")?;
    Ok(())
}
fn only(chairs: &[ChairSession]) -> &ChairSession {
    &chairs[0]
}

#[test]
fn transcript_totals_merge_streamed_messages_and_split_cache_writes() -> Result {
    let temp = tempfile::tempdir()?;
    let config = temp.path();
    let session = "7c1e4b2a-93d5-4f0e-b6a8-2d4c9e1f5a37";
    write(
        &config.join(format!("projects/-work-repo/{session}.jsonl")),
        &[
            r#"{"type":"user","message":{"content":"a prompt that mentions \"usage\""}}"#.into(),
            // The same message streamed twice: the larger counts are final.
            assistant(
                "m1",
                "claude-opus-5-5",
                r#"{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"output_tokens":1}"#,
            ),
            assistant(
                "m1",
                "claude-opus-5-5",
                r#"{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"cache_creation":{"ephemeral_5m_input_tokens":4,"ephemeral_1h_input_tokens":6},"output_tokens":40}"#,
            ),
            assistant(
                "m2",
                "claude-opus-5-5",
                r#"{"input_tokens":2,"cache_read_input_tokens":300,"cache_creation_input_tokens":0,"output_tokens":5}"#,
            ),
            assistant(
                "m3",
                "claude-haiku-5-5",
                r#"{"input_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":2}"#,
            ),
            assistant(
                "m4",
                "<synthetic>",
                r#"{"input_tokens":9,"output_tokens":9}"#,
            ),
            "not json".into(),
        ],
    )?;
    // A sub-agent's messages bill the same account.
    write(
        &config.join(format!(
            "projects/-work-repo/{session}/subagents/agent-1.jsonl"
        )),
        &[assistant(
            "s1",
            "claude-haiku-5-5",
            r#"{"input_tokens":4,"cache_read_input_tokens":40,"cache_creation_input_tokens":0,"output_tokens":3}"#,
        )],
    )?;
    let mut chairs = vec![chair("work", session)];
    attach_usage(&mut chairs, |_| Some(config.to_owned()));
    let chair = only(&chairs);
    assert_eq!(chair.unread, None);
    let opus = &chair.usage[1];
    assert_eq!(opus.model, "claude-opus-5-5");
    assert_eq!(opus.usage.uncached_input, Some(5));
    assert_eq!(opus.usage.cache_read, Some(400));
    assert_eq!(opus.usage.cache_write, Some(10));
    assert_eq!(opus.usage.cache_write_1h, Some(6));
    assert_eq!(opus.usage.output_excluding_reasoning, Some(45));
    let haiku = &chair.usage[0];
    assert_eq!(haiku.model, "claude-haiku-5-5");
    assert_eq!(haiku.usage.uncached_input, Some(5));
    assert_eq!(haiku.usage.cache_read, Some(40));
    // No TTL split was reported for this model.
    assert_eq!(haiku.usage.cache_write_1h, None);
    Ok(())
}

#[test]
fn unreadable_sessions_keep_their_reason() -> Result {
    let temp = tempfile::tempdir()?;
    let config = temp.path();
    std::fs::create_dir_all(config.join("projects/-work"))?;
    let outside = temp.path().join("outside.jsonl");
    write(
        &outside,
        &[assistant("x", "claude-opus-5-5", r#"{"input_tokens":1}"#)],
    )?;
    std::os::unix::fs::symlink(&outside, config.join("projects/-work/linked.jsonl"))?;
    let mut chairs = vec![
        chair("work", "missing"),
        chair("work", "linked"),
        chair("work", "../escape"),
        chair("work", "no-config"),
    ];
    attach_usage(&mut chairs, |chair| {
        (chair.session != "no-config").then(|| config.to_owned())
    });
    let reasons: Vec<_> = chairs.iter().map(|c| c.unread).collect();
    assert_eq!(
        reasons,
        [
            Some(Unread::TranscriptNotFound),
            Some(Unread::TranscriptNotFound),
            Some(Unread::TranscriptNotFound),
            Some(Unread::NoConfigDir),
        ]
    );
    assert!(chairs.iter().all(|c| c.usage.is_empty()));
    Ok(())
}

#[test]
fn a_repeated_session_is_read_once_and_filled_everywhere() -> Result {
    let temp = tempfile::tempdir()?;
    let config = temp.path();
    write(
        &config.join("projects/-p/s1.jsonl"),
        &[assistant(
            "m",
            "claude-sonnet-5-5",
            r#"{"input_tokens":1,"cache_read_input_tokens":2,"cache_creation_input_tokens":3,"output_tokens":4}"#,
        )],
    )?;
    let mut chairs = vec![chair("a", "s1"), chair("a", "s1")];
    attach_usage(&mut chairs, |_| Some(config.to_owned()));
    assert_eq!(chairs[0].usage, chairs[1].usage);
    assert_eq!(chairs[0].usage.len(), 1);
    Ok(())
}

#[test]
fn oversized_lines_are_skipped_without_losing_the_next_record() -> Result {
    let temp = tempfile::tempdir()?;
    let config = temp.path();
    let filler = "x".repeat(MAX_LINE_BYTES + 10);
    write(
        &config.join("projects/-p/s2.jsonl"),
        &[
            format!(r#"{{"type":"user","message":{{"content":"{filler}"}}}}"#),
            assistant(
                "m",
                "claude-sonnet-5-5",
                r#"{"input_tokens":7,"output_tokens":1}"#,
            ),
        ],
    )?;
    let mut chairs = vec![chair("a", "s2")];
    attach_usage(&mut chairs, |_| Some(config.to_owned()));
    assert_eq!(only(&chairs).usage[0].usage.uncached_input, Some(7));
    Ok(())
}
