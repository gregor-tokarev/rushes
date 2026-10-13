//! Rushes desktop application.
// Log through `log_line!`: `eprintln!` panics once the launching terminal is closed.
#![warn(clippy::print_stderr, clippy::print_stdout)]

/// Writes one line to stderr like `eprintln!`, but never panics. Once the
/// terminal that started the app is closed, writing to stderr fails (EIO), and
/// `eprintln!` would take the app down with it, losing edits that are still
/// waiting to be saved. Use this for every log line in the app.
#[macro_export]
macro_rules! log_line {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

pub mod ai_edit;
pub mod database;
pub mod editor;
pub mod onboarding;
pub mod publish;
pub mod route;
pub mod settings_page;
pub mod shell;
pub mod state;
pub mod ui;
pub mod update;
pub mod video_page;
pub mod wizard;
