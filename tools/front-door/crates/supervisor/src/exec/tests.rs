use super::*;
use std::sync::Mutex;
struct Decoder(Mutex<Vec<Vec<u8>>>);
impl FrameSink for Decoder {
    fn try_frame(&self, bytes: &[u8]) -> Result<(), QueueLoss> {
        self.0.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
}
#[test]
fn decoder_sees_terminal_beyond_retention_and_refuses_oversized_frame() {
    let sink = Decoder(Mutex::new(Vec::new()));
    let mut frames = Frames::default();
    let mut capture = CapturedStream::default();
    frames.decode(&vec![b'x'; FRAME_LIMIT + 1], Some(&sink), &mut capture);
    frames.decode(b"\nterminal\n", Some(&sink), &mut capture);
    assert_eq!(capture.oversized_frames, 1);
    assert_eq!(*sink.0.lock().unwrap(), vec![b"terminal".to_vec()]);
}
#[test]
fn tee_has_a_fixed_queue_and_reports_full_and_closed() {
    let (sink, receiver) = tee_queue(1).unwrap();
    assert_eq!(
        sink.try_send(&vec![b'x'; FRAME_LIMIT + 1]),
        Err(QueueLoss::Full)
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(sink.try_send(b"one"), Ok(()));
    assert_eq!(sink.try_send(b"two"), Err(QueueLoss::Full));
    assert_eq!(receiver.try_recv().unwrap(), b"one");
    drop(receiver);
    assert_eq!(sink.try_send(b"three"), Err(QueueLoss::Closed));
    assert!(tee_queue(0).is_err());
    assert!(tee_queue(1025).is_err());
}
#[test]
fn reaper_reserves_before_spawn_and_releases_unused_admission() {
    let reaper = Reaper::new(1).unwrap();
    let first = reaper.reserve().unwrap();
    assert!(reaper.reserve().is_none());
    assert_eq!(reaper.occupied(), 1);
    drop(first);
    assert!(reaper.reserve().is_some());
}

#[test]
fn decoder_and_filter_failures_are_observable_without_fabricating_frames() {
    struct ClosedDecoder;
    impl FrameSink for ClosedDecoder {
        fn try_frame(&self, _: &[u8]) -> Result<(), QueueLoss> {
            Err(QueueLoss::Closed)
        }
    }
    let mut frames = Frames::default();
    let mut capture = CapturedStream::default();
    frames.decode(b"terminal\n", Some(&ClosedDecoder), &mut capture);
    assert_eq!(capture.decoder_lost_frames, 1);

    struct FailedFilter;
    impl CaptureFilter for FailedFilter {
        fn filter(&mut self, _: &[u8]) -> Result<Vec<u8>, FilterError> {
            Err(FilterError)
        }
    }
    let decoder = Decoder(Mutex::new(Vec::new()));
    let mut filter = FailedFilter;
    let mut pump = Pump {
        policy: StreamPolicy::Capture { limit: 32 },
        decoder: Some(&decoder),
        filter: Some(&mut filter),
        frames: Frames::default(),
    };
    let mut pipe = Some(io::Cursor::new(b"terminal\n"));
    let mut capture = CapturedStream::default();
    pump.read(&mut pipe, &mut capture);
    pump.read(&mut pipe, &mut capture);
    assert!(pipe.is_none());
    assert!(capture.filter_failed);
    assert_eq!(capture.total_bytes, 9);
    assert!(capture.bytes.is_empty());
    assert_eq!(*decoder.0.lock().unwrap(), vec![b"terminal".to_vec()]);
}

#[test]
fn pipe_read_failure_is_observable_and_closes_the_failed_stream() {
    struct FailedRead;
    impl Read for FailedRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("injected read failure"))
        }
    }
    let mut pump = Pump {
        policy: StreamPolicy::default(),
        decoder: None,
        filter: None,
        frames: Frames::default(),
    };
    let mut pipe = Some(FailedRead);
    let mut capture = CapturedStream::default();
    pump.read(&mut pipe, &mut capture);
    assert!(pipe.is_none());
    assert!(capture.read_failed);
    assert_eq!(capture.total_bytes, 0);
    assert!(capture.bytes.is_empty());
}
#[test]
fn bounds_and_pre_cancel_refuse_without_spawn() {
    let executor = SystemExecutor::new(1).unwrap();
    let argv = ["/must/not/spawn".into()];
    let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::ZERO);
    options.stdout = StreamPolicy::Capture {
        limit: MAX_CAPTURE_LIMIT + 1,
    };
    assert!(matches!(
        executor.execute(options),
        Err(ExecError::InvalidLimit)
    ));
    let token = CancelToken::new();
    token.cancel();
    let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::ZERO);
    options.cancel = Some(&token);
    assert!(matches!(
        executor.execute(options),
        Err(ExecError::Cancelled)
    ));
    assert!(matches!(
        executor.execute(ExecOptions::new(&[], Path::new("/"), Duration::ZERO)),
        Err(ExecError::EmptyArgv)
    ));
}
