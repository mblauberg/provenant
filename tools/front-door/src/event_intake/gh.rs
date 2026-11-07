//! `gh api` transport for conditional requests.
use super::reconcile::{Forge, Response};
use std::{
    collections::BTreeMap,
    io,
    time::{Duration, Instant},
};

/// A single-worker adapter. It honors per-resource GitHub poll intervals.
#[derive(Default)]
pub struct Gh {
    next: BTreeMap<String, Instant>,
    retry: Option<Instant>,
    path: Option<String>,
}
impl Gh {
    /// Explicit launchd/foreground executable search path, without mutating the
    /// process environment after the webhook thread starts.
    pub fn with_path(path: String) -> Self {
        Self {
            path: Some(path),
            ..Self::default()
        }
    }
}
impl Forge for Gh {
    #[allow(clippy::disallowed_methods, reason = "forge transport timing boundary")]
    fn get(&mut self, path: &str, etag: Option<&str>) -> io::Result<Response> {
        if let Some(delay) = self
            .retry
            .and_then(|at| at.checked_duration_since(Instant::now()))
        {
            std::thread::sleep(delay);
        }
        if let Some(delay) = self
            .next
            .get(path)
            .and_then(|at| at.checked_duration_since(Instant::now()))
        {
            std::thread::sleep(delay);
        }
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor};
        let mut argv: Vec<std::ffi::OsString> = [
            "gh",
            "api",
            "--include",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        if let Some(etag) = etag {
            argv.extend(["-H".into(), format!("If-None-Match: {etag}").into()]);
        }
        argv.push(path.into());
        let mut environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                key != "PROVENANT_INTAKE_SECRET" && !(self.path.is_some() && key == "PATH")
            })
            .collect();
        if let Some(path) = &self.path {
            environment.push(("PATH".into(), path.into()));
        }
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(60));
        options.env = Some(&environment);
        options.process_group = true;
        options.stop_on_output_limit = true;
        let output = SystemExecutor::new(1)
            .map_err(io::Error::other)?
            .execute(options)
            .map_err(io::Error::other)?;
        if output.timed_out || output.stdout.truncated || output.drain_truncated || output.cancelled
        {
            return Err(io::Error::other("gh request did not complete"));
        }
        let response = parse(&output.stdout.bytes)?;
        let retry = response
            .headers
            .get("retry-after")
            .and_then(|s| {
                s.parse::<u64>().ok().or_else(|| {
                    let date = s.strip_suffix(" GMT")?;
                    let at = jiff::Timestamp::strptime(
                        "%a, %d %b %Y %H:%M:%S %z",
                        format!("{date} +0000"),
                    )
                    .ok()?;
                    Some(
                        at.as_second()
                            .saturating_sub(jiff::Timestamp::now().as_second())
                            .max(0) as u64,
                    )
                })
            })
            .or_else(|| {
                if !matches!(response.status, 403 | 429 | 500..=599) {
                    return None;
                }
                let reset = if response
                    .headers
                    .get("x-ratelimit-remaining")
                    .map(String::as_str)
                    == Some("0")
                {
                    response
                        .headers
                        .get("x-ratelimit-reset")
                        .and_then(|s| s.parse::<i64>().ok())
                        .map(|at| {
                            at.saturating_sub(jiff::Timestamp::now().as_second()).max(0) as u64
                        })
                } else {
                    None
                };
                Some(reset.unwrap_or(60))
            });
        if let Some(seconds) = retry {
            self.retry = Some(
                Instant::now()
                    .checked_add(Duration::from_secs(seconds))
                    .ok_or_else(|| io::Error::other("retry interval outside clock range"))?,
            );
        } else {
            self.retry = None;
        }
        if let Some(seconds) = response
            .headers
            .get("x-poll-interval")
            .and_then(|s| s.parse::<u64>().ok())
        {
            let deadline = Instant::now()
                .checked_add(Duration::from_secs(seconds))
                .ok_or_else(|| io::Error::other("poll interval outside clock range"))?;
            self.next.insert(path.into(), deadline);
        }
        Ok(response)
    }
}

fn parse(output: &[u8]) -> io::Result<Response> {
    let (offset, separator) = output
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| output.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)))
        .ok_or_else(|| io::Error::other("gh returned no HTTP headers"))?;
    let text = std::str::from_utf8(&output[..offset]).map_err(io::Error::other)?;
    let mut lines = text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other("gh returned invalid HTTP status"))?;
    let mut headers = BTreeMap::new();
    for line in lines {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("gh returned invalid HTTP header"))?;
        headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
    }
    Ok(Response {
        status,
        headers,
        body: output[offset + separator..].to_vec(),
    })
}

#[cfg(test)]
#[allow(dead_code, unused_imports)] // Also compiled by the harness=false worktree driver, without libtest.
mod tests {
    use super::*;
    #[test]
    fn parses_conditional_response_and_rate_headers() -> io::Result<()> {
        let response = parse(
            b"HTTP/2.0 304 Not Modified\r\nETag: W/\"tag\"\r\nX-RateLimit-Remaining: 7\r\n\r\n",
        )?;
        assert_eq!(response.status, 304);
        assert_eq!(
            response.headers.get("etag").map(String::as_str),
            Some("W/\"tag\"")
        );
        assert_eq!(
            response
                .headers
                .get("x-ratelimit-remaining")
                .map(String::as_str),
            Some("7")
        );
        Ok(())
    }
}
