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
