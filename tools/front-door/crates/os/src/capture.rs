//! Bounded streaming capture, shared by utility execution and provider custody.
//! Provider frames are decoded before filtering and retention truncation.
use crate::exec::{
    CaptureFilter, CapturedStream, ExecError, FRAME_LIMIT, FrameSink, MAX_CAPTURE_LIMIT, QueueLoss,
    StreamPolicy,
};
#[cfg(test)]
use crate::exec::{FilterError, NonblockingSink};
use std::io::{self, Read};
use thiserror::Error;

const INPUT_CHUNK_LIMIT: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameEvent {
    Progress,
    Terminal,
    ProtocolRejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("provider frame could not be decoded")]
pub struct DecodeFailure;

/// Provider-specific parsing returns lifecycle facts only. It has no process
/// or signalling capability and receives complete frames capped at FRAME_LIMIT.
pub trait FrameDecoder: Send + Sync {
    fn decode(&self, frame: &[u8]) -> Result<FrameEvent, DecodeFailure>;
}

/// Bounded, nonblocking delivery of provider facts to Run's event channel.
pub trait DecodedEventSink: Send + Sync {
    fn try_send(&self, event: FrameEvent) -> Result<(), QueueLoss>;
}

/// An S1 frame sink accepts already framed bytes. Provider decoders instead
/// return typed events, which are bridged through the same capture counters.
#[derive(Clone, Copy)]
pub enum FrameDelivery<'a> {
    Raw(&'a dyn FrameSink),
    Provider {
        decoder: &'a dyn FrameDecoder,
        events: &'a dyn DecodedEventSink,
    },
}

#[derive(Default)]
pub(crate) struct Frames {
    bytes: Vec<u8>,
    oversized: bool,
    filter_disabled: bool,
}

/// Incremental stream capture using S1's stream policy, outcome, filter and
/// queue types. Its temporary frame buffer never exceeds 64 KiB.
pub struct StreamCapture<'a, 'f> {
    policy: StreamPolicy<'a>,
    decoder: Option<FrameDelivery<'a>>,
    filter: Option<&'f mut dyn CaptureFilter>,
    frames: Frames,
    finished: bool,
    outcome: CapturedStream,
}
impl<'a, 'f> StreamCapture<'a, 'f> {
    pub fn new(
        policy: StreamPolicy<'a>,
        decoder: Option<FrameDelivery<'a>>,
        filter: Option<&'f mut dyn CaptureFilter>,
    ) -> Result<Self, ExecError> {
        let limit = match &policy {
            StreamPolicy::Capture { limit }
            | StreamPolicy::Tail { limit }
            | StreamPolicy::Tee { limit, .. } => *limit,
            StreamPolicy::Discard => 0,
        };
        if limit > MAX_CAPTURE_LIMIT {
            return Err(ExecError::InvalidLimit);
        }
        Ok(Self {
            policy,
            decoder,
            filter,
            frames: Frames {
                bytes: Vec::with_capacity(FRAME_LIMIT),
                oversized: false,
                filter_disabled: false,
            },
            finished: false,
            outcome: CapturedStream::default(),
        })
    }

    /// Feed raw provider bytes. Internal processing remains bounded even when
    /// the caller supplies a larger buffer.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), ExecError> {
        if self.finished {
            return Err(ExecError::CaptureFinished);
        }
        self.outcome.total_bytes = self.outcome.total_bytes.saturating_add(bytes.len() as u64);
        for chunk in bytes.chunks(INPUT_CHUNK_LIMIT) {
            self.frames
                .decode_delivery(chunk, self.decoder, &mut self.outcome);
            self.frames
                .filter_and_retain(&self.policy, &mut self.filter, chunk, &mut self.outcome);
        }
        Ok(())
    }

    /// EOF flushes a final partial frame. Repeated calls are harmless.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.frames.finish_delivery(self.decoder, &mut self.outcome);
        self.frames
            .finish_filter(&self.policy, &mut self.filter, &mut self.outcome);
        self.finished = true;
    }

    /// Read one nonblocking turn from an executor pipe.
    pub fn read<R: Read>(&mut self, pipe: &mut Option<R>) {
        let Some(reader) = pipe else {
            return;
        };
        let mut buffer = [0; INPUT_CHUNK_LIMIT];
        match reader.read(&mut buffer) {
            Ok(0) => {
                self.finish();
                *pipe = None;
            }
            Ok(count) => {
                if self.push(&buffer[..count]).is_err() {
                    self.outcome.read_failed = true;
                    self.frames
                        .discard_partial_frame(self.decoder, &mut self.outcome);
                    *pipe = None;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => {
                self.outcome.read_failed = true;
                self.frames
                    .discard_partial_frame(self.decoder, &mut self.outcome);
                *pipe = None;
            }
        }
    }

    pub fn outcome(&self) -> &CapturedStream {
        &self.outcome
    }

    /// A drain timeout is not EOF. Do not decode its incomplete final frame.
    /// Count that frame as decoder loss before returning the accumulated result.
    pub fn into_outcome(mut self) -> CapturedStream {
        if !self.finished {
            self.frames
                .discard_partial_frame(self.decoder, &mut self.outcome);
            self.frames
                .finish_filter(&self.policy, &mut self.filter, &mut self.outcome);
        }
        self.outcome
    }
}

impl Frames {
    #[cfg(test)]
    pub(crate) fn decode(
        &mut self,
        chunk: &[u8],
        sink: Option<&dyn FrameSink>,
        out: &mut CapturedStream,
    ) {
        self.decode_delivery(chunk, sink.map(FrameDelivery::Raw), out);
    }

    fn decode_delivery(
        &mut self,
        chunk: &[u8],
        sink: Option<FrameDelivery<'_>>,
        out: &mut CapturedStream,
    ) {
        let Some(sink) = sink else {
            return;
        };
        for &byte in chunk {
            if byte == b'\n' {
                if !self.oversized && self.deliver(&sink).is_err() {
                    out.decoder_lost_frames = out.decoder_lost_frames.saturating_add(1);
                }
                self.bytes.clear();
                self.oversized = false;
            } else if !self.oversized {
                if self.bytes.len() == FRAME_LIMIT {
                    self.bytes.clear();
                    self.oversized = true;
                    out.oversized_frames = out.oversized_frames.saturating_add(1);
                } else {
                    self.bytes.push(byte);
                }
            }
        }
    }

    fn finish_delivery(&mut self, sink: Option<FrameDelivery<'_>>, out: &mut CapturedStream) {
        if !self.bytes.is_empty()
            && !self.oversized
            && sink.is_some_and(|sink| self.deliver(&sink).is_err())
        {
            out.decoder_lost_frames = out.decoder_lost_frames.saturating_add(1);
        }
        self.bytes.clear();
    }

    fn deliver(&self, sink: &FrameDelivery<'_>) -> Result<(), QueueLoss> {
        match sink {
            FrameDelivery::Raw(sink) => sink.try_frame(&self.bytes),
            FrameDelivery::Provider { decoder, events } => match decoder.decode(&self.bytes) {
                Ok(event) => events.try_send(event),
                Err(_) => Err(QueueLoss::Full),
            },
        }
    }

    fn filter_and_retain(
        &mut self,
        policy: &StreamPolicy<'_>,
        filter: &mut Option<&mut dyn CaptureFilter>,
        chunk: &[u8],
        out: &mut CapturedStream,
    ) {
        if self.filter_disabled {
            return;
        }
        let filtered;
        let bytes = if let Some(filter) = filter.as_deref_mut() {
            match filter.filter(chunk) {
                Ok(value) if value.len() <= FRAME_LIMIT => {
                    filtered = value;
                    &filtered
                }
                Err(_) | Ok(_) => {
                    self.filter_disabled = true;
                    out.filter_failed = true;
                    return;
                }
            }
        } else {
            chunk
        };

        self.retain_filtered(policy, bytes, out);
    }

    fn finish_filter(
        &mut self,
        policy: &StreamPolicy<'_>,
        filter: &mut Option<&mut dyn CaptureFilter>,
        out: &mut CapturedStream,
    ) {
        if self.filter_disabled {
            return;
        }
        let Some(filter) = filter.as_deref_mut() else {
            return;
        };
        match filter.finish() {
            Ok(bytes) if bytes.len() <= FRAME_LIMIT => self.retain_filtered(policy, &bytes, out),
            Err(_) | Ok(_) => {
                self.filter_disabled = true;
                out.filter_failed = true;
            }
        }
    }

    fn retain_filtered(&self, policy: &StreamPolicy<'_>, bytes: &[u8], out: &mut CapturedStream) {
        if bytes.is_empty() {
            return;
        }
        if let StreamPolicy::Tee { sink, .. } = policy
            && let Err(loss) = sink.try_send(bytes)
        {
            out.sink_lost_bytes = out.sink_lost_bytes.saturating_add(bytes.len() as u64);
            out.sink_failed |= loss == QueueLoss::Closed;
        }

        if let StreamPolicy::Tail { limit } = policy {
            let discard = out
                .bytes
                .len()
                .saturating_add(bytes.len())
                .saturating_sub(*limit);
            out.truncated |= discard > 0;
            if discard >= out.bytes.len() {
                let skip = discard - out.bytes.len();
                out.bytes.clear();
                out.bytes.extend_from_slice(&bytes[skip..]);
            } else {
                out.bytes.drain(..discard);
                out.bytes.extend_from_slice(bytes);
            }
            return;
        }
        let limit = match policy {
            StreamPolicy::Capture { limit } | StreamPolicy::Tee { limit, .. } => *limit,
            StreamPolicy::Discard | StreamPolicy::Tail { .. } => return,
        };
        let room = limit.saturating_sub(out.bytes.len());
        out.bytes.extend_from_slice(&bytes[..bytes.len().min(room)]);
        out.truncated |= bytes.len() > room;
    }

    fn discard_partial_frame(&mut self, sink: Option<FrameDelivery<'_>>, out: &mut CapturedStream) {
        if sink.is_some() && !self.oversized && !self.bytes.is_empty() {
            out.decoder_lost_frames = out.decoder_lost_frames.saturating_add(1);
        }
        self.bytes.clear();
        self.oversized = false;
    }
}

/// Thin executor adapter retained for S1's focused pipe tests. Production
/// utility and provider paths both use StreamCapture directly.
#[cfg(test)]
pub(crate) struct Pump<'a> {
    pub(crate) policy: StreamPolicy<'a>,
    pub(crate) decoder: Option<&'a dyn FrameSink>,
    pub(crate) filter: Option<&'a mut dyn CaptureFilter>,
    pub(crate) frames: Frames,
}
#[cfg(test)]
impl Pump<'_> {
    pub(crate) fn read<R: Read>(&mut self, pipe: &mut Option<R>, out: &mut CapturedStream) {
        let Some(reader) = pipe else {
            return;
        };
        let mut buffer = [0; INPUT_CHUNK_LIMIT];
        match reader.read(&mut buffer) {
            Ok(0) => {
                self.frames
                    .finish_delivery(self.decoder.map(FrameDelivery::Raw), out);
                self.frames
                    .finish_filter(&self.policy, &mut self.filter, out);
                *pipe = None;
            }
            Ok(count) => {
                let bytes = &buffer[..count];
                out.total_bytes = out.total_bytes.saturating_add(count as u64);
                self.frames
                    .decode_delivery(bytes, self.decoder.map(FrameDelivery::Raw), out);
                self.frames
                    .filter_and_retain(&self.policy, &mut self.filter, bytes, out);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => {
                out.read_failed = true;
                self.frames
                    .discard_partial_frame(self.decoder.map(FrameDelivery::Raw), out);
                *pipe = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct TestDecoder;
    impl FrameDecoder for TestDecoder {
        fn decode(&self, frame: &[u8]) -> Result<FrameEvent, DecodeFailure> {
            match frame {
                b"progress" => Ok(FrameEvent::Progress),
                b"terminal" => Ok(FrameEvent::Terminal),
                b"rejected" => Ok(FrameEvent::ProtocolRejected),
                _ => Err(DecodeFailure),
            }
        }
    }

    #[derive(Default)]
    struct EventSink {
        events: Mutex<Vec<FrameEvent>>,
        fail: Option<QueueLoss>,
    }
    impl DecodedEventSink for EventSink {
        fn try_send(&self, event: FrameEvent) -> Result<(), QueueLoss> {
            if let Some(failure) = self.fail {
                return Err(failure);
            }
            match self.events.lock() {
                Ok(mut events) => {
                    events.push(event);
                    Ok(())
                }
                Err(_) => Err(QueueLoss::Closed),
            }
        }
    }
    impl EventSink {
        fn collected(&self) -> Vec<FrameEvent> {
            match self.events.lock() {
                Ok(events) => events.clone(),
                Err(_) => Vec::new(),
            }
        }
    }

    struct ReplacingFilter;
    impl CaptureFilter for ReplacingFilter {
        fn filter(&mut self, bytes: &[u8]) -> Result<Vec<u8>, FilterError> {
            if bytes == b"secret" {
                Ok(b"[redacted]".to_vec())
            } else {
                Ok(bytes.to_vec())
            }
        }
    }

    struct FailingFilter;
    impl CaptureFilter for FailingFilter {
        fn filter(&mut self, _bytes: &[u8]) -> Result<Vec<u8>, FilterError> {
            Err(FilterError)
        }
    }

    struct FinishingFilter {
        pending: Vec<u8>,
    }
    impl CaptureFilter for FinishingFilter {
        fn filter(&mut self, bytes: &[u8]) -> Result<Vec<u8>, FilterError> {
            self.pending.extend_from_slice(bytes);
            Ok(Vec::new())
        }

        fn finish(&mut self) -> Result<Vec<u8>, FilterError> {
            Ok(std::mem::take(&mut self.pending))
        }
    }

    #[derive(Default)]
    struct Tee {
        sent: Mutex<Vec<Vec<u8>>>,
        fail: Option<QueueLoss>,
    }
    impl NonblockingSink for Tee {
        fn try_send(&self, bytes: &[u8]) -> Result<(), QueueLoss> {
            if let Some(failure) = self.fail {
                return Err(failure);
            }
            match self.sent.lock() {
                Ok(mut sent) => {
                    sent.push(bytes.to_vec());
                    Ok(())
                }
                Err(_) => Err(QueueLoss::Closed),
            }
        }
    }

    fn provider<'a>(decoder: &'a TestDecoder, events: &'a EventSink) -> FrameDelivery<'a> {
        FrameDelivery::Provider { decoder, events }
    }

    #[test]
    fn tail_retains_latest_bytes_across_chunks() {
        let mut capture = StreamCapture::new(StreamPolicy::Tail { limit: 4 }, None, None).unwrap();
        capture.push(b"abc").unwrap();
        capture.push(b"def").unwrap();
        assert_eq!(capture.outcome().bytes, b"cdef");
        assert!(capture.outcome().truncated);
        capture.push(b"123456").unwrap();
        assert_eq!(capture.outcome().bytes, b"3456");
        assert_eq!(capture.outcome().total_bytes, 12);
        let mut empty = StreamCapture::new(StreamPolicy::Tail { limit: 0 }, None, None).unwrap();
        empty.push(b"x").unwrap();
        assert!(empty.outcome().bytes.is_empty());
        assert!(empty.outcome().truncated);
    }

    #[test]
    fn terminal_frame_is_decoded_before_capture_truncation() {
        let decoder = TestDecoder;
        let events = EventSink::default();
        let capture = StreamCapture::new(
            StreamPolicy::Capture { limit: 0 },
            Some(provider(&decoder, &events)),
            None,
        );
        assert!(capture.is_ok());
        let Ok(mut capture) = capture else {
            return;
        };
        assert!(capture.push(b"terminal\n").is_ok());
        capture.finish();
        assert_eq!(capture.outcome().total_bytes, 9);
        assert!(capture.outcome().bytes.is_empty());
        assert!(capture.outcome().truncated);
        assert_eq!(events.collected(), vec![FrameEvent::Terminal]);
    }

    #[test]
    fn split_frames_and_partial_eof_are_decoded() {
        let decoder = TestDecoder;
        let events = EventSink::default();
        let capture = StreamCapture::new(
            StreamPolicy::Discard,
            Some(provider(&decoder, &events)),
            None,
        );
        assert!(capture.is_ok());
        let Ok(mut capture) = capture else {
            return;
        };
        assert!(capture.push(b"pro").is_ok());
        assert!(capture.push(b"gress\nterminal").is_ok());
        capture.finish();
        capture.finish();
        assert_eq!(
            events.collected(),
            vec![FrameEvent::Progress, FrameEvent::Terminal]
        );
        assert_eq!(capture.outcome().total_bytes, 17);
        assert!(capture.outcome().bytes.is_empty());
    }

    #[test]
    fn oversized_frames_are_counted_and_never_decoded_as_terminal() {
        let decoder = TestDecoder;
        let events = EventSink::default();
        let capture = StreamCapture::new(
            StreamPolicy::Capture { limit: 8 },
            Some(provider(&decoder, &events)),
            None,
        );
        assert!(capture.is_ok());
        let Ok(mut capture) = capture else {
            return;
        };
        let mut bytes = vec![b'x'; FRAME_LIMIT + 1];
        bytes.extend_from_slice(b"\nprogress\n");
        assert!(capture.push(&bytes).is_ok());
        capture.finish();
        assert_eq!(capture.outcome().oversized_frames, 1);
        assert_eq!(capture.outcome().decoder_lost_frames, 0);
        assert_eq!(events.collected(), vec![FrameEvent::Progress]);
    }

    #[test]
    fn frame_limit_is_inclusive_and_invalid_frames_are_counted() {
        let decoder = TestDecoder;
        let events = EventSink::default();
        let capture = StreamCapture::new(
            StreamPolicy::Capture { limit: 0 },
            Some(provider(&decoder, &events)),
            None,
        );
        assert!(capture.is_ok());
        let Ok(mut capture) = capture else {
            return;
        };
        let exact = vec![b'x'; FRAME_LIMIT];
        assert!(capture.push(&exact).is_ok());
        assert!(capture.push(b"\ninvalid\n").is_ok());
        capture.finish();
        assert_eq!(capture.outcome().oversized_frames, 0);
        assert_eq!(capture.outcome().decoder_lost_frames, 2);
        assert!(events.collected().is_empty());
    }

    #[test]
    fn decoder_queue_loss_is_recorded_without_stopping_or_losing_capture() {
        let decoder = TestDecoder;
        let events = EventSink {
            events: Mutex::new(Vec::new()),
            fail: Some(QueueLoss::Full),
        };
        let capture = StreamCapture::new(
            StreamPolicy::Capture { limit: 64 },
            Some(provider(&decoder, &events)),
            None,
        );
        assert!(capture.is_ok());
        let Ok(mut capture) = capture else {
            return;
        };
        assert!(capture.push(b"terminal\n").is_ok());
        capture.finish();
        assert_eq!(capture.outcome().decoder_lost_frames, 1);
        assert_eq!(capture.outcome().bytes, b"terminal\n");
        assert!(events.collected().is_empty());
    }

    #[test]
    fn filter_runs_before_retention_and_filter_failure_discards_output() {
        let mut filter = ReplacingFilter;
        let capture =
            StreamCapture::new(StreamPolicy::Capture { limit: 10 }, None, Some(&mut filter));
        assert!(capture.is_ok());
        let Ok(mut capture) = capture else {
            return;
        };
        assert!(capture.push(b"secret").is_ok());
        assert_eq!(capture.outcome().bytes, b"[redacted]");
        assert_eq!(capture.outcome().total_bytes, 6);
        let _outcome = capture.into_outcome();

        let mut filter = FailingFilter;
        let capture =
            StreamCapture::new(StreamPolicy::Capture { limit: 16 }, None, Some(&mut filter));
        assert!(capture.is_ok());
        let Ok(mut capture) = capture else {
            return;
        };
        assert!(capture.push(b"secret").is_ok());
        assert!(capture.outcome().filter_failed);
        assert!(capture.outcome().bytes.is_empty());
    }

    #[test]
    fn eof_flushes_filter_tail_through_normal_retention() {
        let mut filter = FinishingFilter {
            pending: Vec::new(),
        };
        let capture =
            StreamCapture::new(StreamPolicy::Capture { limit: 16 }, None, Some(&mut filter));
        assert!(capture.is_ok());
        let Ok(mut capture) = capture else {
            return;
        };
        assert!(capture.push(b"tail").is_ok());
        assert!(capture.outcome().bytes.is_empty());
        capture.finish();
        assert_eq!(capture.outcome().bytes, b"tail");
        assert!(!capture.outcome().filter_failed);
    }

    #[test]
    fn tee_loss_is_separate_from_retention_and_discard_has_no_pipe() {
        let tee = Tee {
            sent: Mutex::new(Vec::new()),
            fail: Some(QueueLoss::Full),
        };
        let capture = StreamCapture::new(
            StreamPolicy::Tee {
                limit: 3,
                sink: &tee,
            },
            None,
            None,
        );
        assert!(capture.is_ok());
        let Ok(mut capture) = capture else {
            return;
        };
        assert!(capture.push(b"hello").is_ok());
        assert_eq!(capture.outcome().bytes, [b'h', b'e', b'l']);
        assert_eq!(capture.outcome().sink_lost_bytes, 5);
        assert!(capture.outcome().truncated);
        let _outcome = capture.into_outcome();

        assert!(!StreamPolicy::Discard.piped());
    }

    #[test]
    fn bounds_and_finish_state_are_typed() {
        let too_large = StreamCapture::new(
            StreamPolicy::Capture {
                limit: MAX_CAPTURE_LIMIT + 1,
            },
            None,
            None,
        );
        assert!(matches!(too_large, Err(ExecError::InvalidLimit)));

        let capture = StreamCapture::new(StreamPolicy::default(), None, None);
        assert!(capture.is_ok());
        let Ok(mut capture) = capture else {
            return;
        };
        capture.finish();
        assert!(matches!(
            capture.push(b"late"),
            Err(ExecError::CaptureFinished)
        ));
    }
}
