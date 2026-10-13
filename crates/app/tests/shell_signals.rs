//! A termination signal sent to the process reaches the app's quit path.
//! Its own test binary: the handler stays installed for the whole process.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::TestAppContext;
use rushes::shell;

#[gpui_kit::test]
fn sigterm_quits_like_ctrl_q(cx: &mut TestAppContext) {
    // The signal is read on async-io's reactor thread, outside the test scheduler.
    cx.executor().allow_parking();
    let quit = Rc::new(Cell::new(false));
    let flag = quit.clone();
    cx.update(|cx| shell::on_termination_signal(cx, move |_| flag.set(true)));
    cx.run_until_parked();
    assert!(!quit.get());

    let sent =
        std::process::Command::new("kill").args(["-TERM", &std::process::id().to_string()]).status().expect("run kill");
    assert!(sent.success());

    // The signal arrives on another thread; give it a moment.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !quit.get() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        cx.run_until_parked();
    }
    assert!(quit.get(), "the quit path ran");
}
