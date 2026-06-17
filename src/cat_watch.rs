use std::io::Write;
use std::time::Duration;

/// Run a closure in a watch loop.
///
/// Clears the terminal before each run and sleeps `interval_secs` between
/// iterations. Press Ctrl-C (or send SIGINT) to exit — the process terminates
/// naturally, same as the Unix `watch` command.
///
/// When stdout is not a terminal (piped to a file or another command), ANSI
/// clear sequences are omitted so the output stays clean.
pub fn run_watch(interval_secs: u64, f: impl Fn()) {
    // Use a small polling interval (100ms) so the first run appears instantly
    // and we don't accumulate a full interval before the first display.
    let poll_ms = 100u64;
    let steps_per_interval = (interval_secs * 1000).max(poll_ms) / poll_ms;

    let is_tty = atty::is(atty::Stream::Stdout);
    let mut step = 0u64;

    loop {
        if step == 0 {
            // Clear screen and move cursor home
            if is_tty {
                let _ = write!(std::io::stdout(), "\x1b[2J\x1b[H");
                let _ = std::io::stdout().flush();
            }
            f();
            let _ = std::io::stdout().flush();
        }

        step = (step + 1) % steps_per_interval;
        std::thread::sleep(Duration::from_millis(poll_ms));
    }
}

/// Run a closure once with a live-updating header, then exit.
///
/// Unlike `run_watch`, this does not loop — it clears the screen once, runs
/// the closure, and returns. Useful for one-shot refresh without the visual
/// noise of previous terminal content.
pub fn run_watch_once(f: impl Fn()) {
    if atty::is(atty::Stream::Stdout) {
        let _ = write!(std::io::stdout(), "\x1b[2J\x1b[H");
        let _ = std::io::stdout().flush();
    }
    f();
    let _ = std::io::stdout().flush();
}
