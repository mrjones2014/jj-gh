//! Tiny braille spinner for stderr. No-op when stderr is not a TTY (CI, piped
//! output) so logs stay clean.
//!
//! Uses `anstyle` for the dim color escape and `tokio::time` for the tick
//! loop.

use std::io::{IsTerminal, Write};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(100);

/// Animated spinner that runs on a background tokio task. Dropping without
/// calling [`Spinner::stop`] is safe (the task exits on the next tick) but
/// the final cleared line is best-effort in that case.
pub struct Spinner {
    handle: Option<JoinHandle<()>>,
    stop_tx: Option<oneshot::Sender<()>>,
}

impl Spinner {
    /// Start a spinner with `msg`. When stderr is not a terminal, returns a
    /// no-op spinner so callers can use it unconditionally.
    pub fn start(msg: impl Into<String>) -> Self {
        if !std::io::stderr().is_terminal() {
            return Self {
                handle: None,
                stop_tx: None,
            };
        }
        let msg = msg.into();
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let dim = anstyle::Style::new().dimmed();
            let mut interval = tokio::time::interval(TICK);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut frame = 0usize;
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = interval.tick() => {
                        let glyph = FRAMES[frame % FRAMES.len()];
                        frame = frame.wrapping_add(1);
                        let mut err = std::io::stderr().lock();
                        let _ = write!(err, "\r{dim}{glyph} {msg}{dim:#}");
                        let _ = err.flush();
                    }
                }
            }
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        });
        Self {
            handle: Some(handle),
            stop_tx: Some(stop_tx),
        }
    }

    /// Stop the spinner and clear its line. Awaiting ensures the cleared line
    /// is flushed before subsequent output.
    pub async fn stop(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
    }
}
