//! The one-line spinner shown on stderr while `enw` waits for something.

use std::io::{self, IsTerminal, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

pub(crate) const FRAME_INTERVAL: Duration = Duration::from_millis(200);
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Draw `message` over the current line, with the spinner at step `frame` and
/// the whole seconds since `started`.
pub(crate) fn draw_line(out: &mut impl Write, frame: usize, message: &str, started: Instant) {
    let spinner = FRAMES[frame % FRAMES.len()];
    let elapsed = started.elapsed().as_secs();
    let _ = write!(out, "\r\x1b[2K{spinner} {message} ({elapsed}s)");
    let _ = out.flush();
}

pub(crate) fn clear_line(out: &mut impl Write) {
    let _ = write!(out, "\r\x1b[2K");
    let _ = out.flush();
}

/// Run `work`, showing a spinner on stderr if it takes noticeably long and
/// stderr is a terminal.
pub(crate) fn with_spinner<T>(message: &str, work: impl FnOnce() -> T) -> T {
    let mut stderr = io::stderr();
    if !stderr.is_terminal() {
        return work();
    }
    spin_while(&mut stderr, message, FRAME_INTERVAL, work)
}

/// Run `work` on this thread while another draws the spinner into `out`.
/// Nothing is drawn for work that finishes within one `interval`, so instant
/// work does not flash a line; whatever was drawn is cleared before
/// returning.
fn spin_while<T>(
    out: &mut (impl Write + Send),
    message: &str,
    interval: Duration,
    work: impl FnOnce() -> T,
) -> T {
    let started = Instant::now();
    // Dropping the sender, on return or unwind, is what stops the spinner.
    let (running, stopped) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let mut frame = 0;
            while stopped.recv_timeout(interval) == Err(RecvTimeoutError::Timeout) {
                draw_line(out, frame, message, started);
                frame += 1;
            }
            if frame > 0 {
                clear_line(out);
            }
        });
        let _running = running;
        work()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instant_work_draws_nothing() {
        let mut out = Vec::new();

        let result = spin_while(&mut out, "working", Duration::from_secs(60), || 42);

        assert_eq!(result, 42);
        assert!(out.is_empty(), "{:?}", String::from_utf8_lossy(&out));
    }

    #[test]
    fn slow_work_shows_the_message_and_clears_the_line_afterwards() {
        let mut out = Vec::new();

        spin_while(&mut out, "working", Duration::from_millis(1), || {
            std::thread::sleep(Duration::from_millis(100))
        });

        let drawn = String::from_utf8(out).unwrap();
        assert!(drawn.starts_with("\r\x1b[2K⠋ working (0s)"), "{drawn:?}");
        assert!(drawn.ends_with("\r\x1b[2K"), "{drawn:?}");
    }
}
