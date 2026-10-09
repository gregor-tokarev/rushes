//! Termination signals: SIGTERM (logout, `systemctl stop`, `kill`), SIGINT
//! (Ctrl+C in a terminal) and SIGHUP (the terminal closed) quit like Ctrl+Q,
//! so the quit hooks write edits that are still waiting for their save delay.

use gpui_kit::App;

/// Calls `quit` on the first termination signal; a second one while the app
/// is still shutting down ends the process at once. Call once at startup.
#[cfg(unix)]
pub fn on_termination_signal(cx: &mut App, quit: impl FnOnce(&mut App) + 'static) {
    use async_signal::{Signal, Signals};
    use futures::StreamExt as _;

    let mut signals = match Signals::new([Signal::Term, Signal::Int, Signal::Hup]) {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("videocrm: cannot listen for termination signals: {error}");
            return;
        }
    };
    let (received, first) = futures::channel::oneshot::channel();
    // Listened to off the main thread, which stops running tasks once it starts quitting.
    cx.background_executor()
        .spawn(async move {
            let Some(Ok(signal)) = signals.next().await else { return };
            eprintln!("videocrm: {signal:?} received, saving and quitting");
            _ = received.send(());
            if let Some(Ok(signal)) = signals.next().await {
                eprintln!("videocrm: {signal:?} received again, quitting now");
                std::process::exit(128 + signal as i32);
            }
        })
        .detach();
    cx.spawn(async move |cx| {
        if first.await.is_ok() {
            cx.update(quit);
        }
    })
    .detach();
}

/// Termination signals are a Unix concept; elsewhere the window's close and
/// Quit are the ways out.
#[cfg(not(unix))]
pub fn on_termination_signal(_: &mut App, _: impl FnOnce(&mut App) + 'static) {}
