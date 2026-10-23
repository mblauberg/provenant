//! Utility signals are never provider StopReason values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    Terminate,
    Kill,
}
