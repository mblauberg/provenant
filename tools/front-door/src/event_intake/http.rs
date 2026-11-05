//! Bounded HTTP/1.1 webhook request handling for a local tunnel endpoint.
use super::{Intake, WebhookResponse, receive};
use std::io::{self, Read, Write};

const MAX_HEADERS: usize = 16 * 1024;
const MAX_BODY: usize = 1024 * 1024;

/// Handle exactly one HTTP request and close the connection. The caller sets
/// a read timeout and serialises access to `intake` across connections.
pub fn handle(stream: &mut (impl Read + Write), intake: &mut Intake, now: u64) -> io::Result<()> {
    let code = match read_request(stream) {
        Ok((headers, body)) => {
            let borrowed: Vec<(&str, &str)> = headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            match receive(intake, &borrowed, &body, now) {
                Ok(WebhookResponse::Accepted) => 202,
                Ok(WebhookResponse::Rejected) => 401,
                Err(_) => 503,
            }
        }
        Err(_) => 400,
    };
    let reason = match code {
        202 => "Accepted",
        401 => "Unauthorized",
        503 => "Service Unavailable",
        _ => "Bad Request",
    };
    write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()
}

type Request = (Vec<(String, String)>, Vec<u8>);

#[allow(clippy::disallowed_methods, reason = "HTTP request deadline boundary")]
fn read_request(stream: &mut impl Read) -> io::Result<Request> {
    let started = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(3);
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        if started.elapsed() >= timeout {
            return Err(io::Error::other("request deadline exceeded"));
        }
        if bytes.len() >= MAX_HEADERS {
            return Err(io::Error::other("headers too large"));
        }
        stream.read_exact(&mut byte)?;
        bytes.push(byte[0]);
    }
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let mut lines = text.split("\r\n");
    if lines.next() != Some("POST /webhook HTTP/1.1") {
        return Err(io::Error::other("invalid request line"));
    }
    let mut headers = Vec::new();
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::other("invalid header"))?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(io::Error::other("invalid header name"));
        }
        let value = value.trim();
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(io::Error::other("transfer encoding unsupported"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(io::Error::other("duplicate length"));
            }
            length = Some(value.parse::<usize>().map_err(io::Error::other)?);
        }
        headers.push((name.to_owned(), value.to_owned()));
    }
    let length = length.ok_or_else(|| io::Error::other("missing content length"))?;
    if length > MAX_BODY {
        return Err(io::Error::other("body too large"));
    }
    let mut body = vec![0; length];
    let mut read = 0;
    while read < length {
        if started.elapsed() >= timeout {
            return Err(io::Error::other("request deadline exceeded"));
        }
        let count = stream.read(&mut body[read..])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete body",
            ));
        }
        read += count;
    }
    Ok((headers, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    struct Connection {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Connection {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }
    impl Write for Connection {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn handles_signed_requests_and_rejects_ambiguous_lengths() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut intake = Intake::open(dir.path().join("intake"), "o/r".into(), b"secret".to_vec())?;
        let body = r#"{"repository":{"full_name":"o/r"},"pull_request":{"number":12}}"#;
        let headers = format!(
            "POST /webhook HTTP/1.1\r\nContent-Length: {}\r\nX-GitHub-Delivery: id\r\nX-GitHub-Event: pull_request\r\nX-Hub-Signature-256: sha256=db4061419529a8addfd7db7f148ff8677579ec4723d46ba890f9c7c80f8629ab\r\n",
            body.len()
        );
        let unsigned = format!(
            "POST /webhook HTTP/1.1\r\nContent-Length: {}\r\nX-GitHub-Delivery: id\r\nX-GitHub-Event: pull_request\r\n",
            body.len()
        );
        for header in [
            String::new(),
            "X-Hub-Signature-256: sha256=bad\r\n".to_owned(),
        ] {
            let mut conn = Connection {
                input: Cursor::new(format!("{unsigned}{header}\r\n{body}").into_bytes()),
                output: Vec::new(),
            };
            handle(&mut conn, &mut intake, 9)?;
            assert!(conn.output.starts_with(b"HTTP/1.1 401 "));
        }
        assert!(intake.markers().is_empty());
        for _ in 0..2 {
            let mut conn = Connection {
                input: Cursor::new(format!("{headers}\r\n{body}").into_bytes()),
                output: Vec::new(),
            };
            handle(&mut conn, &mut intake, 10)?;
            assert!(conn.output.starts_with(b"HTTP/1.1 202 "));
        }
        assert_eq!(intake.markers().len(), 1);
        let mut conn = Connection {
            input: Cursor::new(
                format!("{headers}Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes(),
            ),
            output: Vec::new(),
        };
        handle(&mut conn, &mut intake, 10)?;
        assert!(conn.output.starts_with(b"HTTP/1.1 400 "));
        Ok(())
    }
}
