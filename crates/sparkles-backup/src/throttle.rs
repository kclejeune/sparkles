//! Bandwidth limits: a token bucket per repository and direction, shared by all its
//! tasks (`maxUploadBytesPerSec`, `maxDownloadBytesPerSec`).

use crate::Ctl;
use crate::error::Result;

/// A token bucket of `rate` bytes per second (burst: one second's worth, at least one
/// piece). `None`: unlimited.
#[derive(Debug)]
pub struct Throttle {
    pub(crate) rate: Option<u64>,
}

impl Throttle {
    pub fn new(bytes_per_sec: Option<u64>) -> Throttle {
        Throttle {
            rate: bytes_per_sec.filter(|r| *r > 0),
        }
    }

    pub fn unlimited() -> Throttle {
        Throttle { rate: None }
    }

    pub fn rate(&self) -> Option<u64> {
        self.rate
    }

    /// Wait until `bytes` may be transferred. Waits in slices of at most 100 ms and
    /// fails with `cancelled` as soon as `ctl` is cancelled. Returns immediately when
    /// unlimited.
    ///
    /// Until the token bucket is implemented, every call returns immediately.
    pub async fn take(&self, bytes: u64, ctl: &Ctl) -> Result<()> {
        let _ = bytes;
        ctl.check()
    }
}
