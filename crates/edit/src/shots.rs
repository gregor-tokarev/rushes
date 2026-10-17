//! Shots inside a stretch of a take: where the picture cuts, and how much of the frame
//! moves in each part. Tells the payoff of a silent screen demo (a video playing full
//! screen) from the preparation around it (opening a page, a small player, the cursor,
//! switching windows), so the agent keeps only the part worth watching.

use std::fmt::Write as _;
use std::path::Path;

use crate::error::{EditError, Result};
use crate::tool;

const RATE: f64 = 5.0;
const WIDTH: usize = 96;
const HEIGHT: usize = 54;
/// A pixel changed between two frames (0-255 grey).
const PIXEL_CHANGE: u8 = 6;
/// Mean change of a hard cut.
const HARD_CUT: f32 = 30.0;
/// Mean change clear enough to put a shot boundary on.
const VISIBLE_CHANGE: f32 = 8.0;
/// Shots shorter than this join a neighbour.
const MIN_SHOT: f64 = 2.5;
pub const MAX_SPAN: f64 = 600.0;
const MAX_TILES: usize = 16;

/// Share of the frame (averaged over [`WINDOW`]) that changes in full-screen motion.
const BUSY: f32 = 0.15;
/// Seconds of motion averaged before deciding.
const WINDOW: f64 = 2.0;
/// A full-screen video that calms down (a fade, a still moment) is still the video when
/// the picture cuts away within this many seconds.
const SETTLE: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Motion {
    /// The screen mostly stays: a still page, a cursor, scrolling, a small player.
    Calm,
    /// Most of the screen moves: full-screen video or action.
    Busy,
}

impl Motion {
    fn describe(self) -> &'static str {
        match self {
            Self::Calm => "the screen mostly stays (a still page, a cursor, scrolling, a small player)",
            Self::Busy => "most of the screen moves (full-screen video or action)",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    pub start: f64,
    pub end: f64,
    motion: Motion,
    /// The picture cuts hard into this shot.
    cut_in: bool,
}

impl Shot {
    /// Most of the screen moves: a full-screen video or action.
    pub fn moving(&self) -> bool {
        self.motion == Motion::Busy
    }
}

/// The shots of `file` between `start` and `end` seconds.
pub fn shots(file: &Path, start: f64, end: f64) -> Result<Vec<Shot>> {
    if end.partial_cmp(&start) != Some(std::cmp::Ordering::Greater) || end - start > MAX_SPAN {
        return Err(EditError::invalid(format!("shots: give a range of up to {MAX_SPAN:.0} s (end after start)")));
    }
    let raw = tool::run(
        &tool::ffmpeg()?,
        [
            "-nostdin".as_ref(),
            "-v".as_ref(),
            "error".as_ref(),
            "-ss".as_ref(),
            tool::secs(start).as_ref(),
            "-t".as_ref(),
            tool::secs(end - start).as_ref(),
            "-i".as_ref(),
            file.as_os_str(),
            "-map".as_ref(),
            "0:v:0".as_ref(),
            "-vf".as_ref(),
            format!("fps={RATE},scale={WIDTH}:{HEIGHT}:flags=area,format=gray").as_ref(),
            "-f".as_ref(),
            "rawvideo".as_ref(),
            "-".as_ref(),
        ],
    )?;
    let frames: Vec<&[u8]> = raw.as_chunks::<{ WIDTH * HEIGHT }>().0.iter().map(|f| f.as_slice()).collect();
    Ok(segment(&frames, start, end))
}

fn segment(frames: &[&[u8]], start: f64, end: f64) -> Vec<Shot> {
    if frames.len() < 2 {
        return vec![Shot { start, end, motion: Motion::Calm, cut_in: false }];
    }
    let size = (WIDTH * HEIGHT) as f32;
    let mut area = Vec::with_capacity(frames.len());
    let mut change = Vec::with_capacity(frames.len());
    let mut cut = Vec::with_capacity(frames.len());
    for pair in frames.windows(2) {
        let (mut sum, mut moved) = (0u32, 0u32);
        for (a, b) in pair[0].iter().zip(pair[1]) {
            let d = a.abs_diff(*b);
            sum += u32::from(d);
            moved += u32::from(d > PIXEL_CHANGE);
        }
        area.push(moved as f32 / size);
        change.push(sum as f32 / size);
        cut.push(sum as f32 / size >= HARD_CUT);
    }
    let half = ((WINDOW * RATE) as usize / 2).max(1);
    let motion: Vec<Motion> = (0..area.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half).min(area.len());
            let mean = area[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
            if mean >= BUSY { Motion::Busy } else { Motion::Calm }
        })
        .collect();
    let time = |i: usize| (start + (i as f64 + 1.0) / RATE).min(end);
    let mut shots: Vec<Shot> = vec![Shot { start, end: start, motion: motion[0], cut_in: false }];
    for i in 0..motion.len() {
        let last = shots.last_mut().expect("one shot");
        // Hard cuts split calm stretches (another page, another window); inside
        // full-screen video they are the video's own cuts.
        let hard = cut[i] && motion[i] == Motion::Calm;
        if hard || motion[i] != last.motion {
            last.end = time(i);
            shots.push(Shot { start: time(i), end: time(i), motion: motion[i], cut_in: hard });
        }
    }
    shots.last_mut().expect("one shot").end = end;
    // A busy shot that calms down without a cut, and cuts away soon after, ends at that cut.
    let mut i = 1;
    while i + 1 < shots.len() {
        let calm = &shots[i];
        if shots[i - 1].motion == Motion::Busy
            && shots[i - 1].end - shots[i - 1].start >= MIN_SHOT
            && calm.motion == Motion::Calm
            && !calm.cut_in
            && calm.end - calm.start < SETTLE
            && shots[i + 1].cut_in
        {
            shots[i - 1].end = calm.end;
            shots.remove(i);
        } else {
            i += 1;
        }
    }
    // Short shots join the longer neighbour.
    while shots.len() > 1 {
        let Some(index) = (0..shots.len())
            .filter(|i| shots[*i].end - shots[*i].start < MIN_SHOT)
            .min_by(|a, b| (shots[*a].end - shots[*a].start).total_cmp(&(shots[*b].end - shots[*b].start)))
        else {
            break;
        };
        let length = |i: usize| shots[i].end - shots[i].start;
        let into = match (index.checked_sub(1), index + 1 < shots.len()) {
            (Some(previous), true) => {
                if length(previous) >= length(index + 1) {
                    previous
                } else {
                    index + 1
                }
            }
            (Some(previous), false) => previous,
            (None, _) => index + 1,
        };
        let shot = shots.remove(index);
        let target = if into > index { into - 1 } else { into };
        shots[target].start = shots[target].start.min(shot.start);
        shots[target].end = shots[target].end.max(shot.end);
        if into < index {
            // The absorbed shot came after the target: the target keeps its own start.
        } else {
            shots[target].cut_in = shot.cut_in;
        }
    }
    // Neighbours that ended up alike, without a hard cut between them, become one shot.
    let mut merged: Vec<Shot> = Vec::new();
    for shot in shots {
        match merged.last_mut() {
            Some(last) if last.motion == shot.motion && !shot.cut_in => last.end = shot.end,
            _ => merged.push(shot),
        }
    }
    // The averaging blurs where a shot begins: move each boundary to the biggest picture
    // change near it (going full screen, switching windows), when there is a clear one.
    let frame_at = |time: f64| ((time - start) * RATE).round() as isize - 1;
    for i in 1..merged.len() {
        let at = frame_at(merged[i].start);
        let lo = (at - half as isize).max(0) as usize;
        let hi = ((at + half as isize).max(0) as usize).min(change.len().saturating_sub(1));
        if lo > hi {
            continue;
        }
        let best = (lo..=hi).max_by(|a, b| change[*a].total_cmp(&change[*b])).expect("non-empty");
        if change[best] >= VISIBLE_CHANGE {
            let moved = time(best).clamp(merged[i - 1].start, merged[i].end);
            merged[i - 1].end = moved;
            merged[i].start = moved;
        }
    }
    merged
}

/// The shot list as the agent reads it.
pub fn describe(shots: &[Shot]) -> String {
    let mut out = String::new();
    for (index, shot) in shots.iter().enumerate() {
        let _ = writeln!(
            out,
            "#{:<2} {:.2}-{:.2} ({:.1} s): {}",
            index + 1,
            shot.start,
            shot.end,
            shot.end - shot.start,
            shot.motion.describe()
        );
    }
    out
}

/// Frames to show: the middle of every shot, and more along long shots (one about
/// every 15 s), at most 16.
pub fn sheet_times(shots: &[Shot]) -> Vec<f64> {
    let mut times = Vec::new();
    for shot in shots {
        let length = shot.end - shot.start;
        let count = ((length / 15.0).floor() as usize + 1).min(4);
        for k in 0..count {
            times.push(shot.start + length * (k as f64 + 0.5) / count as f64);
        }
    }
    if times.len() > MAX_TILES {
        let step = times.len() as f64 / MAX_TILES as f64;
        times = (0..MAX_TILES).map(|i| times[(i as f64 * step) as usize]).collect();
    }
    times
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames where `moving` of the pixels change from one frame to the next.
    fn frames(seconds: f64, moving: f32, seed: u8) -> Vec<Vec<u8>> {
        let n = (seconds * RATE) as usize;
        let size = WIDTH * HEIGHT;
        let changing = (size as f32 * moving) as usize;
        (0..n)
            .map(|i| {
                (0..size).map(|p| if p < changing { ((i * 37 + p) % 2) as u8 * 60 + seed } else { seed }).collect()
            })
            .collect()
    }

    #[test]
    fn a_page_then_full_screen_video_then_still_are_three_shots() {
        let mut all = frames(10.0, 0.08, 10);
        all.extend(frames(12.0, 0.6, 10));
        all.extend(frames(5.0, 0.0, 200));
        let refs: Vec<&[u8]> = all.iter().map(Vec::as_slice).collect();
        let shots = segment(&refs, 100.0, 127.0);
        let kinds: Vec<Motion> = shots.iter().map(|s| s.motion).collect();
        assert_eq!(kinds, [Motion::Calm, Motion::Busy, Motion::Calm], "{shots:?}");
        assert!((shots[1].start - 110.0).abs() < 0.5, "{shots:?}");
        assert!((shots[2].start - 122.0).abs() < 0.5, "{shots:?}");
        assert!(describe(&shots).contains("#2 "));
    }

    #[test]
    fn a_video_that_calms_down_before_cutting_away_ends_at_the_cut() {
        let mut all = frames(10.0, 0.0, 10);
        all.extend(frames(12.0, 0.6, 10));
        // The video's last clips: a slow fade, then a still clip, then back to the page.
        all.extend(frames(2.0, 0.0, 120));
        all.extend(frames(1.2, 0.0, 180));
        all.extend(frames(8.0, 0.0, 10));
        let refs: Vec<&[u8]> = all.iter().map(Vec::as_slice).collect();
        let shots = segment(&refs, 0.0, 33.2);
        let kinds: Vec<Motion> = shots.iter().map(|s| s.motion).collect();
        assert_eq!(kinds, [Motion::Calm, Motion::Busy, Motion::Calm], "{shots:?}");
        assert!((shots[1].end - 25.2).abs() < 0.3, "{shots:?}");
    }
}
