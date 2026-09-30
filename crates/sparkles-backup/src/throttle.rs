//! Bandwidth limits: a token bucket per repository and direction, shared by all its
//! tasks (`maxUploadBytesPerSec`, `maxDownloadBytesPerSec`).

use crate::Ctl;
use crate::error::Result;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Longest single sleep, so cancellation is noticed quickly.
const SLICE: Duration = Duration::from_millis(100);

/// A token bucket of `rate` bytes per second (burst: one second's worth). `None`:
/// unlimited. A request larger than the burst takes its tokens one burst at a time.
#[derive(Debug)]
pub struct Throttle {
    pub(crate) rate: Option<u64>,
    /// (tokens available, when they were counted)
    bucket: Mutex<(f64, Instant)>,
}

impl Throttle {
    pub fn new(bytes_per_sec: Option<u64>) -> Throttle {
        let rate = bytes_per_sec.filter(|r| *r > 0);
        Throttle {
            rate,
            // start full: a first request of up to one second's worth goes at once
            bucket: Mutex::new((rate.unwrap_or(0) as f64, Instant::now())),
        }
    }

    pub fn unlimited() -> Throttle {
        Throttle::new(None)
    }

    pub fn rate(&self) -> Option<u64> {
        self.rate
    }

    /// Take up to `want` tokens (at most one burst) if they are there: `Ok(n)`
    /// taken, else `Err(wait)` until enough have accumulated.
    fn try_take(&self, rate: u64, want: u64) -> std::result::Result<u64, Duration> {
        let burst = rate as f64;
        let mut b = self.bucket.lock().unwrap();
        let now = Instant::now();
        b.0 = (b.0 + now.duration_since(b.1).as_secs_f64() * burst).min(burst);
        b.1 = now;
        let want = want.min(rate) as f64;
        if b.0 >= want {
            b.0 -= want;
            Ok(want as u64)
        } else {
            Err(Duration::from_secs_f64((want - b.0) / burst))
        }
    }

    /// Wait until `bytes` may be transferred. Waits in slices of at most 100 ms and
    /// fails with `cancelled` as soon as `ctl` is cancelled. Returns immediately when
    /// unlimited.
    pub async fn take(&self, bytes: u64, ctl: &Ctl) -> Result<()> {
        ctl.check()?;
        let Some(rate) = self.rate else {
            return Ok(());
        };
        let mut left = bytes;
        while left > 0 {
            match self.try_take(rate, left) {
                Ok(n) => left -= n,
                Err(wait) => {
                    tokio::time::sleep(wait.min(SLICE)).await;
                    ctl.check()?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn limits_the_rate() {
        let t = Throttle::new(Some(100_000));
        let ctl = Ctl::default();
        let start = Instant::now();
        // one burst at once, then 100 KB/s
        t.take(100_000, &ctl).await.unwrap();
        assert!(start.elapsed() < Duration::from_millis(50));
        t.take(30_000, &ctl).await.unwrap();
        let e = start.elapsed();
        assert!(e >= Duration::from_millis(250), "{e:?}");
        assert!(e < Duration::from_secs(2), "{e:?}");
        // unlimited never waits
        let u = Throttle::unlimited();
        u.take(u64::MAX, &ctl).await.unwrap();
        assert_eq!(u.rate(), None);
        assert_eq!(Throttle::new(Some(0)).rate(), None);
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_wait() {
        let t = Arc::new(Throttle::new(Some(1000)));
        let ctl = Ctl::default();
        let c2 = ctl.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            c2.cancel.store(true, Ordering::Relaxed);
        });
        let start = Instant::now();
        let e = t.take(1_000_000, &ctl).await.unwrap_err();
        assert!(e.is_cancelled());
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
