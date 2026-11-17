//! Source spans bridge decoded detector decisions back to unchanged input bytes.
use super::{
    DecisionQueue, FRAME_LIMIT, FilterError, FrameEmitter, REDACTED, Redactor, RedactorError,
};
use std::collections::VecDeque;

// One decoded detector can withhold a capture-frame authority or a bounded
// literal prefix, plus a decoded piece. Unlike repeated raw backslashes, mixed
// spellings of withheld decoded backslashes cannot always be compressed. Bound
// those mappings explicitly, as the token scanner bounds undecided authorities.
const MAX_SOURCE_SPANS: usize = FRAME_LIMIT * 2;

pub(super) type DecisionSink<'a> = dyn FnMut(u8, usize, bool) -> Result<(), FilterError> + 'a;

pub(super) fn emit(
    byte: u8,
    count: usize,
    masked: bool,
    redaction_run: &mut bool,
    out: &mut FrameEmitter<'_>,
) {
    if masked {
        if !*redaction_run {
            out.push_bytes(REDACTED);
        }
    } else {
        out.push_repeat(byte, count);
    }
    *redaction_run = masked;
}

struct Span {
    source: Vec<u8>,
    decoded_len: usize,
    count: usize,
    decided: usize,
    masked: bool,
}

/// Runs compress arbitrarily long repeated delimiters withheld by a detector.
/// Otherwise storage is bounded by the detector's undecided prefix and the
/// caller's bounded decoded piece. A Unicode escape maps to one indivisible span.
#[derive(Default)]
pub(super) struct SourceMap {
    spans: VecDeque<Span>,
}
impl SourceMap {
    pub(super) fn push(&mut self, source: &[u8], decoded_len: usize) -> Result<(), FilterError> {
        if let Some(last) = self.spans.back_mut()
            && last.source == source
            && last.decoded_len == decoded_len
        {
            last.count += 1;
        } else {
            if self.spans.len() == MAX_SOURCE_SPANS {
                return Err(FilterError);
            }
            self.spans.push_back(Span {
                source: source.to_vec(),
                decoded_len,
                count: 1,
                decided: 0,
                masked: false,
            });
        }
        Ok(())
    }

    pub(super) fn decide(
        &mut self,
        mut count: usize,
        masked: bool,
        sink: &mut DecisionSink<'_>,
    ) -> Result<(), FilterError> {
        if count == 0 {
            return sink(0, 0, masked);
        }
        while count > 0 {
            let span = self.spans.front_mut().ok_or(FilterError)?;
            let used = count.min(span.decoded_len - span.decided);
            span.decided += used;
            span.masked |= masked;
            count -= used;
            if span.decided == span.decoded_len {
                for &byte in &span.source {
                    sink(byte, 1, span.masked)?;
                }
                span.count -= 1;
                span.decided = 0;
                span.masked = false;
                if span.count == 0 {
                    self.spans.pop_front();
                }
            }
        }
        Ok(())
    }

    pub(super) fn finish(&self) -> Result<(), FilterError> {
        if self.spans.is_empty() {
            Ok(())
        } else {
            Err(FilterError)
        }
    }
}

/// Receives each decoded byte run with the source bytes it came from.
type SpanSink<'a> = dyn FnMut(&[u8], &[u8]) -> Result<(), FilterError> + 'a;

#[derive(Default)]
struct Unescape {
    // At most one surrogate pair, including both source escapes (12 bytes).
    pending: Vec<u8>,
}
impl Unescape {
    fn push(&mut self, byte: u8, sink: &mut SpanSink<'_>) -> Result<(), FilterError> {
        self.pending.push(byte);
        self.drain(false, sink)
    }
    fn drain(&mut self, eof: bool, sink: &mut SpanSink<'_>) -> Result<(), FilterError> {
        while !self.pending.is_empty() {
            let p = &self.pending;
            if p[0] == b'\\' && p.len() == 1 && !eof {
                break;
            }
            let mut width = 1;
            let mut decoded = None;
            if p[0] == b'\\' && p.len() >= 2 {
                if b"\"\\/bfnrt".contains(&p[1]) {
                    width = 2;
                } else if p[1] == b'u' {
                    if p.len() < 6 && p[2..].iter().all(u8::is_ascii_hexdigit) && !eof {
                        break;
                    }
                    if p.len() >= 6 && p[2..6].iter().all(u8::is_ascii_hexdigit) {
                        let high =
                            u16::from_str_radix(std::str::from_utf8(&p[2..6]).unwrap_or(""), 16)
                                .unwrap_or(0);
                        width = if (0xd800..=0xdbff).contains(&high) {
                            12
                        } else {
                            6
                        };
                        if p.len() < width && !eof {
                            break;
                        }
                    }
                }
                if width > 1 && p.len() >= width {
                    let mut quoted = vec![b'"'];
                    quoted.extend_from_slice(&p[..width]);
                    quoted.push(b'"');
                    decoded = serde_json::from_slice::<String>(&quoted).ok();
                }
            }
            if let Some(decoded) = decoded {
                sink(&p[..width], decoded.as_bytes())?;
                self.pending.drain(..width);
            } else {
                // Invalid or incomplete escapes are ordinary source bytes.
                sink(&p[..1], &p[..1])?;
                self.pending.remove(0);
            }
        }
        Ok(())
    }
}

pub(super) struct EscapedScanner {
    inner: Box<Redactor>,
    unescape: Unescape,
    source: SourceMap,
    pub(super) insertions: Vec<usize>,
}
impl EscapedScanner {
    pub(super) fn new(secrets: Vec<Vec<u8>>) -> Result<Self, RedactorError> {
        Ok(Self {
            inner: Box::new(Redactor::with_escapes(secrets, false)?),
            unescape: Unescape::default(),
            source: SourceMap::default(),
            insertions: Vec::new(),
        })
    }
    fn decoded(&mut self, bytes: &[u8], output: &mut DecisionQueue) -> Result<(), FilterError> {
        self.inner.filter_decisions(bytes, &mut |_, count, masked| {
            self.source
                .decide(count, masked, &mut |byte, count, masked| {
                    if count == 0 && masked {
                        self.insertions.push(output.total);
                    } else {
                        output.push_count(byte, count, masked);
                    }
                    Ok(())
                })
        })
    }
    pub(super) fn filter(
        &mut self,
        bytes: &[u8],
        output: &mut DecisionQueue,
    ) -> Result<(), FilterError> {
        let mut decoded = Vec::new();
        for &byte in bytes {
            self.unescape.push(byte, &mut |source, bytes| {
                self.source.push(source, bytes.len())?;
                decoded.extend_from_slice(bytes);
                Ok(())
            })?;
            if decoded.len() >= 4096 {
                self.decoded(&decoded, output)?;
                decoded.clear();
            }
        }
        self.decoded(&decoded, output)
    }
    pub(super) fn finish(&mut self, output: &mut DecisionQueue) -> Result<(), FilterError> {
        let mut decoded = Vec::new();
        self.unescape.drain(true, &mut |source, bytes| {
            self.source.push(source, bytes.len())?;
            decoded.extend_from_slice(bytes);
            Ok(())
        })?;
        self.decoded(&decoded, output)?;
        self.inner.finish_decisions(&mut |_, count, masked| {
            self.source
                .decide(count, masked, &mut |byte, count, masked| {
                    if count == 0 && masked {
                        self.insertions.push(output.total);
                    } else {
                        output.push_count(byte, count, masked);
                    }
                    Ok(())
                })
        })?;
        self.source.finish()
    }
}
