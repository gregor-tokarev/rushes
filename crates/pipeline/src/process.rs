//! The agent's process group as the app can find it again after a crash.
//!
//! On Linux the agent's group dies with the app: its leader gets `SIGTERM` when the
//! app's agent thread goes away, even when the app is killed, and passes it on to the
//! whole group. [`AgentProcess`] identifies the group for what that cannot cover (a
//! process that ignores `SIGTERM`), so a restarted app can stop it with
//! [`stop_leftover_agent`].

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The agent's process group: the id of its leader, which is also the group id, and
/// when the leader started, to tell it from a later process that got the same id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProcess {
    pub pgid: u32,
    /// The boot the leader started in (Linux `boot_id`); `None` where unknown.
    #[serde(default)]
    pub boot_id: Option<String>,
    /// The leader's start time in clock ticks after boot (Linux); `None` where unknown.
    #[serde(default)]
    pub start_ticks: Option<u64>,
}

impl AgentProcess {
    /// The group led by `pid`, with what identifies its leader.
    pub fn of(pid: u32) -> AgentProcess {
        let stat = imp::stat(pid);
        AgentProcess { pgid: pid, boot_id: imp::boot_id(), start_ticks: stat.map(|stat| stat.start_ticks) }
    }

    /// Whether the group's leader is still the process this describes.
    fn leader_is_alive(&self) -> bool {
        let (Some(boot_id), Some(start_ticks)) = (&self.boot_id, self.start_ticks) else {
            return false;
        };
        imp::boot_id().as_ref() == Some(boot_id)
            && imp::stat(self.pgid)
                .is_some_and(|stat| stat.start_ticks == start_ticks && stat.pgrp == self.pgid && !stat.zombie)
    }
}

/// Stops what is left of an agent's process group after the app that started it died
/// (a crash, `kill -9`): `SIGTERM` to the group, `SIGKILL` to whatever is left after
/// `grace`. Only while the group's leader is still that agent's (same boot, same
/// start time); returns whether it was. Blocks up to `grace`.
pub fn stop_leftover_agent(process: &AgentProcess, grace: Duration) -> bool {
    if !process.leader_is_alive() {
        return false;
    }
    imp::stop_group(process.pgid, grace);
    true
}

#[cfg(target_os = "linux")]
mod imp {
    use std::thread;
    use std::time::{Duration, Instant};

    const POLL_INTERVAL: Duration = Duration::from_millis(20);

    pub(super) struct Stat {
        pub pgrp: u32,
        pub start_ticks: u64,
        pub zombie: bool,
    }

    /// Fields of `/proc/<pid>/stat`: the process group, the start time and whether the
    /// process has exited without being reaped.
    pub(super) fn stat(pid: u32) -> Option<Stat> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name may contain spaces and parentheses; the fields follow the last `)`.
        let (_, fields) = stat.rsplit_once(')')?;
        let fields: Vec<&str> = fields.split_whitespace().collect();
        // Field 3 (`state`) is fields[0], so field n is fields[n - 3].
        Some(Stat {
            zombie: matches!(*fields.first()?, "Z" | "X"),
            pgrp: fields.get(2)?.parse().ok()?,
            start_ticks: fields.get(19)?.parse().ok()?,
        })
    }

    pub(super) fn boot_id() -> Option<String> {
        let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        Some(id.trim().to_owned()).filter(|id| !id.is_empty())
    }

    pub(super) fn stop_group(pgid: u32, grace: Duration) {
        let group = pgid as libc::pid_t;
        signal_group(group, libc::SIGTERM);
        let deadline = Instant::now() + grace;
        while signal_group(group, 0) {
            if Instant::now() >= deadline {
                signal_group(group, libc::SIGKILL);
                return;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn signal_group(group: libc::pid_t, signal: libc::c_int) -> bool {
        // SAFETY: killpg has no memory-safety preconditions. The caller checked that the
        // group's leader is the agent's, so the id is not a recycled one; the kernel does
        // not hand it out again while any member of the group is alive.
        unsafe { libc::killpg(group, signal) == 0 }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::time::Duration;

    pub(super) struct Stat {
        pub pgrp: u32,
        pub start_ticks: u64,
        pub zombie: bool,
    }

    pub(super) fn stat(_pid: u32) -> Option<Stat> {
        None
    }

    pub(super) fn boot_id() -> Option<String> {
        None
    }

    pub(super) fn stop_group(_pgid: u32, _grace: Duration) {}
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_leftover_group_is_stopped_only_while_its_leader_is_the_agent() {
        let mut leader = Command::new("sleep").arg("60").process_group(0).spawn().expect("spawn sleep");
        let process = AgentProcess::of(leader.id());
        assert!(process.start_ticks.is_some() && process.boot_id.is_some(), "{process:?}");
        // Reaped as soon as it exits, as init reaps an orphan.
        let reaper = thread::spawn(move || leader.wait());

        // A process that got the same id later started at another time.
        let other = AgentProcess { start_ticks: process.start_ticks.map(|ticks| ticks + 1), ..process.clone() };
        assert!(!stop_leftover_agent(&other, Duration::from_millis(100)));
        assert!(process.leader_is_alive(), "a stranger's process is left alone");

        let started = Instant::now();
        assert!(stop_leftover_agent(&process, Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(2), "SIGTERM was enough");
        let status = reaper.join().unwrap().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert!(!stop_leftover_agent(&process, Duration::from_millis(100)), "gone");
    }
}
