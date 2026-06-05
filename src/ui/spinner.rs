//! Tiny braille spinner for stderr. No-op when stderr is not a TTY (CI, piped
//! output) so logs stay clean.
//!
//! Uses `anstyle` for the dim color escape and a background thread for the tick
//! loop.

use std::io::{IsTerminal, Write};
use std::sync::{
    Arc, Mutex,
    mpsc::{self, RecvTimeoutError},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(100);

/// Animated spinner that runs on a background thread. Dropping without calling
/// [`Spinner::stop`] is safe: the thread is stopped and the line is cleared
/// synchronously, so `?`/`bail!` paths do not leave stale spinner output.
pub struct Spinner {
    handle: Option<JoinHandle<()>>,
    stop_tx: Option<mpsc::Sender<()>>,
    msg: Arc<Mutex<String>>,
}

impl Spinner {
    /// Start a spinner with `msg`. When stderr is not a terminal, returns a
    /// no-op spinner so callers can use it unconditionally.
    pub fn start(msg: impl Into<String>) -> Self {
        let msg = Arc::new(Mutex::new(msg.into()));
        if !std::io::stderr().is_terminal() {
            return Self {
                handle: None,
                stop_tx: None,
                msg,
            };
        }
        let task_msg = Arc::clone(&msg);
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            let dim = anstyle::Style::new().dimmed();
            let mut frame = 0usize;
            loop {
                match stop_rx.recv_timeout(TICK) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {
                        let glyph = FRAMES[frame % FRAMES.len()];
                        frame = frame.wrapping_add(1);
                        let current = task_msg.lock().expect("spinner msg poisoned").clone();
                        let mut err = std::io::stderr().lock();
                        let _ = write!(err, "\r\x1b[2K{dim}{glyph} {current}{dim:#}");
                        let _ = err.flush();
                    }
                }
            }
            clear_line();
        });
        Self {
            handle: Some(handle),
            stop_tx: Some(stop_tx),
            msg,
        }
    }

    /// Replace the message shown next to the spinner glyph. No-op when stderr
    /// is not a terminal.
    pub fn set_message(&self, msg: String) {
        *self.msg.lock().expect("spinner msg poisoned") = msg;
    }

    /// Stop the spinner and clear its line before subsequent output.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
            clear_line();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

fn clear_line() {
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "\r\x1b[2K");
    let _ = err.flush();
}
