//! Progress reporting and cooperative cancellation.
//!
//! Operations take a [`Progress`] handle. Frontends attach a callback to
//! receive throttled [`ProgressUpdate`]s and keep a clone of the
//! [`CancelToken`] to stop the work from another thread.

use std::cell::RefCell;
use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::{OxideError, Result};

/// A shareable flag that asks a running operation to stop.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// Creates a token in the "not cancelled" state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation. Safe to call from any thread, any number of times.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Returns [`OxideError::Cancelled`] if cancellation was requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() { Err(OxideError::Cancelled) } else { Ok(()) }
    }
}

/// Which operation a [`ProgressUpdate`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// Reading the archive index.
    List,
    /// Writing files out of an archive.
    Extract,
    /// Writing a new archive.
    Create,
    /// Verifying checksums without writing anything.
    Test,
}

/// A snapshot of an operation's progress.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressUpdate {
    /// What is running.
    pub operation: Operation,
    /// The entry currently being processed.
    pub current: Option<String>,
    /// Entries finished so far.
    pub entries_done: u64,
    /// Total entries, when known up front.
    pub entries_total: Option<u64>,
    /// Uncompressed bytes processed so far.
    pub bytes_done: u64,
    /// Total uncompressed bytes, when known up front.
    pub bytes_total: Option<u64>,
    /// Completion between 0 and 1, when it can be estimated.
    pub fraction: Option<f64>,
    /// Time since the operation started.
    #[serde(serialize_with = "serialize_secs")]
    pub elapsed: Duration,
}

fn serialize_secs<S: serde::Serializer>(d: &Duration, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}

impl ProgressUpdate {
    /// Average throughput in bytes per second.
    pub fn bytes_per_second(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs > 0.0 { self.bytes_done as f64 / secs } else { 0.0 }
    }

    /// Estimated time remaining, once enough progress has been made to guess.
    pub fn eta(&self) -> Option<Duration> {
        let fraction = self.fraction?;
        if !(0.01..1.0).contains(&fraction) || self.elapsed < Duration::from_millis(500) {
            return None;
        }
        let total = self.elapsed.as_secs_f64() / fraction;
        Some(Duration::from_secs_f64((total - self.elapsed.as_secs_f64()).max(0.0)))
    }
}

type Callback = Arc<dyn Fn(&ProgressUpdate) + Send + Sync>;

/// Progress sink and cancellation handle passed to every long-running operation.
#[derive(Clone, Default)]
pub struct Progress {
    cancel: CancelToken,
    callback: Option<Callback>,
    interval: Option<Duration>,
}

impl std::fmt::Debug for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Progress")
            .field("cancelled", &self.cancel.is_cancelled())
            .field("has_callback", &self.callback.is_some())
            .finish()
    }
}

impl Progress {
    /// A handle that reports nothing and is never cancelled externally.
    pub fn none() -> Self {
        Self::default()
    }

    /// Reports updates to `callback`, throttled to roughly 20 per second.
    pub fn with_callback(callback: impl Fn(&ProgressUpdate) + Send + Sync + 'static) -> Self {
        Progress { callback: Some(Arc::new(callback)), ..Self::default() }
    }

    /// Uses `token` for cancellation.
    pub fn cancel_with(mut self, token: CancelToken) -> Self {
        self.cancel = token;
        self
    }

    /// Changes the minimum interval between callback invocations.
    pub fn throttle(mut self, interval: Duration) -> Self {
        self.interval = Some(interval);
        self
    }

    /// The cancellation token for this operation.
    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }
}

/// Internal bookkeeping that turns raw counters into throttled updates.
///
/// Uses interior mutability so several readers (e.g. a 7z solid block's
/// inputs) can report into the same tracker.
pub(crate) struct Tracker<'a> {
    progress: &'a Progress,
    start: Instant,
    interval: Duration,
    state: RefCell<TrackerState>,
}

struct TrackerState {
    last_emit: Option<Instant>,
    update: ProgressUpdate,
    input_done: u64,
    input_total: u64,
}

impl<'a> Tracker<'a> {
    pub(crate) fn new(progress: &'a Progress, operation: Operation) -> Self {
        Tracker {
            progress,
            start: Instant::now(),
            interval: progress.interval.unwrap_or(Duration::from_millis(50)),
            state: RefCell::new(TrackerState {
                last_emit: None,
                update: ProgressUpdate {
                    operation,
                    current: None,
                    entries_done: 0,
                    entries_total: None,
                    bytes_done: 0,
                    bytes_total: None,
                    fraction: None,
                    elapsed: Duration::ZERO,
                },
                input_done: 0,
                input_total: 0,
            }),
        }
    }

    pub(crate) fn cancel(&self) -> &CancelToken {
        &self.progress.cancel
    }

    pub(crate) fn check_cancel(&self) -> Result<()> {
        self.progress.cancel.check()
    }

    pub(crate) fn set_totals(&self, entries: Option<u64>, bytes: Option<u64>) {
        let mut s = self.state.borrow_mut();
        s.update.entries_total = entries;
        s.update.bytes_total = bytes;
    }

    /// Sets the archive size, so completion can be estimated from the
    /// compressed input consumed when uncompressed totals are unknown.
    pub(crate) fn set_input_total(&self, total: u64) {
        self.state.borrow_mut().input_total = total;
    }

    /// Records compressed input read by a [`CountingReader`]. Fails once
    /// cancellation is requested, which interrupts even a long skip over
    /// a single huge entry.
    fn add_input(&self, n: u64) -> io::Result<()> {
        if self.progress.cancel.is_cancelled() {
            return Err(io::Error::other("operation cancelled"));
        }
        self.state.borrow_mut().input_done += n;
        self.emit();
        Ok(())
    }

    pub(crate) fn start_entry(&self, name: &str) {
        self.state.borrow_mut().update.current = Some(name.to_owned());
        self.emit();
    }

    pub(crate) fn finish_entry(&self) {
        self.state.borrow_mut().update.entries_done += 1;
        self.emit();
    }

    pub(crate) fn add_bytes(&self, n: u64) {
        self.state.borrow_mut().update.bytes_done += n;
        self.emit();
    }

    pub(crate) fn bytes_done(&self) -> u64 {
        self.state.borrow().update.bytes_done
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    pub(crate) fn finish(&self) {
        let mut s = self.state.borrow_mut();
        s.update.current = None;
        s.update.fraction = Some(1.0);
        s.update.elapsed = self.start.elapsed();
        if let Some(cb) = &self.progress.callback {
            cb(&s.update);
        }
    }

    fn emit(&self) {
        let Some(cb) = &self.progress.callback else {
            return;
        };
        let mut s = self.state.borrow_mut();
        let now = Instant::now();
        if s.last_emit.is_some_and(|t| now.duration_since(t) < self.interval) {
            return;
        }
        s.last_emit = Some(now);
        s.update.elapsed = now.duration_since(self.start);
        s.update.fraction = match s.update.bytes_total {
            Some(total) if total > 0 => Some((s.update.bytes_done as f64 / total as f64).min(1.0)),
            _ if s.input_total > 0 => Some((s.input_done as f64 / s.input_total as f64).min(1.0)),
            _ => match (s.update.entries_total, s.update.entries_done) {
                (Some(total), done) if total > 0 => Some(done as f64 / total as f64),
                _ => None,
            },
        };
        cb(&s.update);
    }
}

/// Reports compressed bytes read from the archive file to a [`Tracker`].
pub(crate) struct CountingReader<'t, R> {
    inner: R,
    tracker: &'t Tracker<'t>,
}

impl<'t, R> CountingReader<'t, R> {
    pub(crate) fn new(inner: R, tracker: &'t Tracker<'t>) -> Self {
        CountingReader { inner, tracker }
    }
}

impl<R: Read> Read for CountingReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.tracker.add_input(n as u64)?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta_requires_progress() {
        let mut u = ProgressUpdate {
            operation: Operation::Extract,
            current: None,
            entries_done: 0,
            entries_total: None,
            bytes_done: 50,
            bytes_total: Some(100),
            fraction: Some(0.5),
            elapsed: Duration::from_secs(10),
        };
        let eta = u.eta().expect("eta");
        assert!((eta.as_secs_f64() - 10.0).abs() < 0.01);
        assert!((u.bytes_per_second() - 5.0).abs() < 1e-9);
        u.fraction = None;
        assert!(u.eta().is_none());
    }

    #[test]
    fn cancel_token_is_shared() {
        let t = CancelToken::new();
        let clone = t.clone();
        assert!(t.check().is_ok());
        clone.cancel();
        assert!(matches!(t.check(), Err(OxideError::Cancelled)));
    }
}
