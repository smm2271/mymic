use anyhow::{Result, bail};
use serde::Serialize;
use std::time::{Duration, Instant};
pub const PERIOD: Duration = Duration::from_millis(10);
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Current,
    Absolute,
    Period,
    Waitable,
}
impl Mode {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "current" => Ok(Self::Current),
            "absolute" => Ok(Self::Absolute),
            "period" => Ok(Self::Period),
            "waitable" => Ok(Self::Waitable),
            _ => bail!("timing-mode must be current, absolute, period or waitable"),
        }
    }
}
pub struct DeadlineClock {
    pub start: Instant,
    pub next_index: u64,
}
impl DeadlineClock {
    pub fn new(start: Instant) -> Self {
        Self {
            start,
            next_index: 0,
        }
    }
    pub fn deadline(&self) -> Instant {
        self.start + Duration::from_millis(self.next_index.saturating_mul(10))
    }
    /// Discard expired slots. Never emit a backlog of stale audio.
    pub fn due(&mut self, now: Instant) -> (u64, u64, Duration) {
        let expected = self.deadline();
        let current = (now.saturating_duration_since(self.start).as_millis() / 10) as u64;
        let slot = self.next_index.max(current);
        let skipped = slot - self.next_index;
        let delay = now.saturating_duration_since(expected);
        self.next_index = slot + 1;
        (slot, skipped, delay)
    }
}
pub struct Waiter {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}
impl Waiter {
    pub fn new() -> Result<Self> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::*;
            // SAFETY: unnamed timer, no security attributes; this thread owns the handle.
            let handle = unsafe {
                CreateWaitableTimerExW(
                    std::ptr::null(),
                    std::ptr::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS,
                )
            };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(Self { handle })
        }
        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }
    pub fn until(&self, deadline: Instant) -> Result<()> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::*;
            let due = -(remaining.as_nanos().div_ceil(100).min(i64::MAX as u128) as i64);
            // SAFETY: live owned handle and initialized relative due time; no callback or APC.
            if unsafe { SetWaitableTimer(self.handle, &due, 0, None, std::ptr::null(), 0) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // SAFETY: handle is valid, wait is bounded to one second, no spinning.
            let result = unsafe { WaitForSingleObject(self.handle, 1000) };
            if result != 0 {
                bail!("high-resolution wait failed: {result}")
            }
        }
        #[cfg(not(windows))]
        std::thread::sleep(remaining);
        Ok(())
    }
}
#[cfg(windows)]
impl Drop for Waiter {
    fn drop(&mut self) {
        // SAFETY: release the unique owned timer after this thread has finished waiting.
        unsafe {
            windows_sys::Win32::System::Threading::CancelWaitableTimer(self.handle);
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}
#[derive(Default, Clone, Copy, Serialize)]
pub struct Counts {
    pub generated_packets: u64,
    pub successfully_sent_packets: u64,
    pub send_errors: u64,
    pub generated_samples: u64,
    pub missed_deadlines: u64,
    pub sequence_gaps_generated_by_tx: u64,
    pub intentional_loss_drops: u64,
    pub queue_drops: u64,
    pub diagnostics_dropped: u64,
}
#[derive(Serialize)]
pub struct Distribution {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}
pub fn distribution(values: &[f64]) -> Option<Distribution> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    Some(Distribution {
        p50: at(0.5),
        p95: at(0.95),
        p99: at(0.99),
        max: *v.last().unwrap(),
    })
}
#[derive(Serialize)]
pub struct Report {
    pub timing_mode: Mode,
    pub mmcss_active: bool,
    pub stream_epoch: u64,
    pub session_id: String,
    pub elapsed_seconds: f64,
    pub interval_seconds: f64,
    pub actual_packets_per_second: f64,
    pub actual_samples_per_second: f64,
    pub send_interval_ms: Option<Distribution>,
    pub scheduling_delay_ms: Option<Distribution>,
    pub scheduling_drift_ms: f64,
    pub counts: Counts,
    pub send_interval_samples_ms: Vec<f64>,
    pub scheduling_delay_samples_ms: Vec<f64>,
    pub final_report: bool,
}
/// Thread-scoped multimedia scheduling, no persistent OS setting changes.
pub struct MultimediaThread {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    pub active: bool,
}
impl MultimediaThread {
    pub fn register() -> Self {
        #[cfg(windows)]
        {
            let name: [u16; 6] = [65, 117, 100, 105, 111, 0];
            let mut index = 0;
            // SAFETY: valid NUL-terminated task name; handle belongs to the calling TX thread.
            let handle = unsafe {
                windows_sys::Win32::System::Threading::AvSetMmThreadCharacteristicsW(
                    name.as_ptr(),
                    &mut index,
                )
            };
            Self {
                handle,
                active: !handle.is_null(),
            }
        }
        #[cfg(not(windows))]
        {
            Self { active: false }
        }
    }
}
#[cfg(windows)]
impl Drop for MultimediaThread {
    fn drop(&mut self) {
        if self.active {
            // SAFETY: reverting the registration on the same thread that acquired it.
            unsafe {
                windows_sys::Win32::System::Threading::AvRevertMmThreadCharacteristics(self.handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadlines_do_not_accumulate_send_time() {
        let start = Instant::now();
        let mut c = DeadlineClock::new(start);
        for n in 0..1000 {
            let (slot, skipped, _) = c.due(start + Duration::from_millis(n * 10 + 2));
            assert_eq!(slot, n);
            assert_eq!(skipped, 0);
            assert_eq!(c.deadline(), start + Duration::from_millis((n + 1) * 10));
        }
    }
    #[test]
    fn stalled_sender_skips_instead_of_bursting() {
        let s = Instant::now();
        let mut c = DeadlineClock::new(s);
        assert_eq!(c.due(s).0, 0);
        let (slot, skipped, _) = c.due(s + Duration::from_millis(85));
        assert_eq!((slot, skipped), (8, 7));
        assert_eq!(c.deadline(), s + Duration::from_millis(90));
    }
    #[test]
    fn quantiles() {
        let d = distribution(&[1.0, 2.0, 3.0, 4.0, 5.0]).unwrap();
        assert_eq!(d.p50, 3.0);
        assert_eq!(d.max, 5.0);
        assert!(distribution(&[]).is_none());
    }
}
