//! Captions in output time, from the words the cut keeps: an SRT next to the render
//! (YouTube takes it as a caption track) and an ASS file for burning in.

use std::fmt::Write as _;

use crate::render::Placed;
use crate::session::Session;
use crate::transcript::Word;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Readable lines of up to ~42 characters, sentence case: long-form videos.
    Sentence,
    /// One to three words, upper case, large: Shorts and fast edits.
    Punchy,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// The kept words in output time.
pub fn output_words(session: &Session, placed: &[Placed]) -> Vec<Word> {
    let mut out = Vec::new();
    for item in placed {
        let Some(view) = session.view(&item.span.take) else { continue };
        for word in view.transcript.words.iter().filter(|w| !w.event && !w.is_filler()) {
            let middle = (word.start + word.end) / 2.0;
            if middle < item.span.start || middle >= item.span.end {
                continue;
            }
            let shift = item.out_start - item.span.start;
            out.push(Word {
                text: word.text.clone(),
                start: (word.start.max(item.span.start) + shift).max(0.0),
                end: word.end.min(item.span.end) + shift,
                event: false,
                speaker: word.speaker.clone(),
            });
        }
    }
    out
}

pub fn cues(words: &[Word], style: Style) -> Vec<Cue> {
    let (max_chars, max_words, pause) = match style {
        Style::Sentence => (42, 12, 0.6),
        Style::Punchy => (16, 3, 0.3),
    };
    let mut cues: Vec<Cue> = Vec::new();
    let mut current: Vec<&Word> = Vec::new();
    let flush = |current: &mut Vec<&Word>, cues: &mut Vec<Cue>| {
        if current.is_empty() {
            return;
        }
        let mut text = current.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        if style == Style::Punchy {
            text = text.trim_end_matches([',', ';', ':']).to_uppercase();
        }
        cues.push(Cue { start: current[0].start, end: current[current.len() - 1].end, text });
        current.clear();
    };
    for (index, word) in words.iter().enumerate() {
        let length: usize =
            current.iter().map(|w| w.text.chars().count() + 1).sum::<usize>() + word.text.chars().count();
        if !current.is_empty() && (length > max_chars || current.len() >= max_words) {
            flush(&mut current, &mut cues);
        }
        current.push(word);
        let next_gap = words.get(index + 1).map_or(f64::INFINITY, |next| next.start - word.end);
        if word.ends_sentence() || next_gap >= pause {
            flush(&mut current, &mut cues);
        }
    }
    flush(&mut current, &mut cues);
    // Readable minimum on screen, without overlapping the next cue.
    let min = match style {
        Style::Sentence => 0.8,
        Style::Punchy => 0.35,
    };
    for index in 0..cues.len() {
        let limit = cues.get(index + 1).map_or(f64::INFINITY, |next| next.start - 0.02);
        let cue = &mut cues[index];
        cue.end = cue.end.max(cue.start + min).min(limit).max(cue.start + 0.05);
    }
    cues
}

pub fn srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (index, cue) in cues.iter().enumerate() {
        let _ = write!(out, "{}\n{} --> {}\n{}\n\n", index + 1, srt_time(cue.start), srt_time(cue.end), cue.text);
    }
    out
}

fn srt_time(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// An ASS script for `width`x`height` output.
pub fn ass(cues: &[Cue], style: Style, width: u32, height: u32) -> String {
    let (size, margin) = match style {
        Style::Sentence => ((height / 18).min(width / 24), height / 14),
        // Big, but a dozen capital letters still fit a line of a vertical frame.
        Style::Punchy => ((height / 11).min(width / 10), height / 4),
    };
    let outline = (size / 12).max(2);
    let mut out = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {width}\nPlayResY: {height}\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n\
         [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Default,Sans,{size},&H00FFFFFF,&H00FFFFFF,&H00000000,&H64000000,1,0,0,0,100,100,0,0,1,{outline},0,2,{side},{side},{margin},1\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        side = width / 12,
    );
    for cue in cues {
        let text = cue.text.replace('\n', "\\N").replace('{', "(").replace('}', ")");
        let _ = writeln!(out, "Dialogue: 0,{},{},Default,,0,0,0,,{text}", ass_time(cue.start), ass_time(cue.end));
    }
    out
}

fn ass_time(seconds: f64) -> String {
    let cs = (seconds.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.into(), start, end, event: false, speaker: None }
    }

    #[test]
    fn sentences_break_on_full_stops_pauses_and_length() {
        let words = vec![
            word("Hello", 0.0, 0.3),
            word("there.", 0.3, 0.6),
            word("This", 0.7, 0.9),
            word("goes", 0.9, 1.1),
            word("on", 1.1, 1.2),
            word("after", 2.0, 2.3),
            word("a", 2.3, 2.4),
            word("pause", 2.4, 2.8),
        ];
        let cues = cues(&words, Style::Sentence);
        let texts: Vec<&str> = cues.iter().map(|cue| cue.text.as_str()).collect();
        assert_eq!(texts, ["Hello there.", "This goes on", "after a pause"]);
        assert!(cues[0].end <= cues[1].start, "no overlap");
        assert!(srt(&cues).starts_with("1\n00:00:00,000 --> 00:00:00,680\nHello there.\n"), "{}", srt(&cues));
    }

    #[test]
    fn punchy_cues_are_short_and_upper_case() {
        let words =
            vec![word("one", 0.0, 0.2), word("two", 0.2, 0.4), word("three,", 0.4, 0.6), word("four", 0.6, 0.8)];
        let texts: Vec<String> = cues(&words, Style::Punchy).into_iter().map(|cue| cue.text).collect();
        assert_eq!(texts, ["ONE TWO THREE", "FOUR"]);
    }
}
