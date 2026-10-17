//! Word-level transcripts and the phrase view the agent reads.
//!
//! The agent never types timestamps for speech: it reads phrases with ids (`A12`),
//! picks which to keep, and the cut plan turns ids into cut times. Hints (false
//! starts, lines said again a bit later, fillers) are computed here so the agent
//! does not have to spot them by reading.

use std::fmt::Write as _;
use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::error::{EditError, Result};

/// A silence at least this long between two spoken words starts a new phrase.
pub const PHRASE_GAP: f64 = 0.5;
/// How far ahead (seconds) a line said again counts as a retake.
const RETAKE_WINDOW: f64 = 120.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// A sound tag such as `[laughs]`, not speech.
    #[serde(default)]
    pub event: bool,
    #[serde(default)]
    pub speaker: Option<String>,
}

impl Word {
    pub fn is_filler(&self) -> bool {
        !self.event && is_filler(&self.text)
    }

    /// A word cut off mid-way (`expl--`): the speaker restarted.
    pub fn is_cut_off(&self) -> bool {
        let text = self.text.trim_end_matches(['.', ',', '!', '?', '…', ':', ';', '"', '»', ')']);
        text.ends_with("--") || (text.ends_with('-') && text.chars().rev().nth(1).is_some_and(char::is_alphanumeric))
    }

    /// Ends a sentence: `.`, `?`, `!` or `…`.
    pub fn ends_sentence(&self) -> bool {
        self.text.trim_end_matches(['"', '»', ')', '\'']).ends_with(['.', '?', '!', '…'])
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub language: Option<String>,
    /// Spoken words and sound tags in time order.
    pub words: Vec<Word>,
}

impl Transcript {
    /// Reads an ElevenLabs Scribe response (`words[]` with `type` word, spacing or
    /// audio_event).
    pub fn from_scribe(json: &[u8]) -> Result<Self> {
        #[derive(Deserialize)]
        struct Scribe {
            language_code: Option<String>,
            #[serde(default)]
            words: Vec<ScribeWord>,
        }
        #[derive(Deserialize)]
        struct ScribeWord {
            text: Option<String>,
            start: Option<f64>,
            end: Option<f64>,
            #[serde(rename = "type")]
            kind: Option<String>,
            speaker_id: Option<String>,
        }
        let scribe: Scribe = serde_json::from_slice(json)?;
        let mut words: Vec<Word> = scribe
            .words
            .into_iter()
            .filter_map(|word| {
                let event = match word.kind.as_deref() {
                    Some("spacing") => return None,
                    Some("audio_event") => true,
                    _ => false,
                };
                let text = word.text?.trim().to_owned();
                let (start, end) = (word.start?, word.end?);
                (!text.is_empty()).then_some(Word { text, start, end: end.max(start), event, speaker: word.speaker_id })
            })
            .collect();
        words.sort_by(|a, b| a.start.total_cmp(&b.start));
        Ok(Self { language: scribe.language_code, words })
    }

    /// Indices of the spoken words (no sound tags).
    pub fn speech(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().filter(|(_, word)| !word.event).map(|(index, _)| index)
    }
}

/// `э-э-э`, `um`, `ммм`: hesitation sounds that carry no meaning. Real short words
/// (Russian `а`, English `a`) are not fillers.
pub fn is_filler(text: &str) -> bool {
    let word: String = text
        .chars()
        .filter(|c| c.is_alphabetic() || *c == '-')
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if word.is_empty() {
        return false;
    }
    const EXACT: [&str; 14] = ["um", "umm", "uh", "uhm", "uhh", "erm", "er", "hmm", "hm", "mm", "mmm", "эм", "э", "хм"];
    if EXACT.contains(&word.as_str()) {
        return true;
    }
    // Held or repeated sounds: э-э-э, ээээ, а-а-а, м-м-м, ммм, uh-uh.
    let letters: Vec<char> = word.chars().filter(|c| *c != '-').collect();
    let all_same = letters.windows(2).all(|pair| pair[0] == pair[1]);
    let held = matches!(letters.first(), Some('э' | 'а' | 'м' | 'e' | 'a' | 'm' | 'u' | 'ы'));
    all_same && held && (letters.len() >= 2)
}

/// A run of words without a pause of [`PHRASE_GAP`] or more, from one speaker.
#[derive(Clone, Debug, PartialEq)]
pub struct Phrase {
    /// `A12`: take id and 1-based number.
    pub id: String,
    /// Indices into [`Transcript::words`], first to last spoken word (sound tags
    /// between them included).
    pub words: Range<usize>,
    pub start: f64,
    pub end: f64,
    /// Silence since the previous phrase (from the start of the take for the first).
    pub gap_before: f64,
    /// Sound tags in that silence.
    pub events_before: Vec<usize>,
}

pub fn phrases(take: &str, transcript: &Transcript) -> Vec<Phrase> {
    let words = &transcript.words;
    let mut phrases: Vec<Phrase> = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    let mut pending_events = Vec::new();
    let mut previous_end = 0.0;
    let push = |phrases: &mut Vec<Phrase>, (first, last): (usize, usize), events: Vec<usize>, previous_end: f64| {
        phrases.push(Phrase {
            id: format!("{take}{}", phrases.len() + 1),
            words: first..last + 1,
            start: words[first].start,
            end: words[last].end,
            gap_before: (words[first].start - previous_end).max(0.0),
            events_before: events,
        });
    };
    for (index, word) in words.iter().enumerate() {
        if word.event {
            if current.is_none() {
                pending_events.push(index);
            }
            continue;
        }
        match current {
            Some((first, last)) => {
                let gap = word.start - words[last].end;
                let new_speaker =
                    word.speaker.is_some() && words[last].speaker.is_some() && word.speaker != words[last].speaker;
                if gap >= PHRASE_GAP || new_speaker {
                    // Sound tags after the last word belong to the silence before the next phrase.
                    let events: Vec<usize> = (last + 1..index).filter(|i| words[*i].event).collect();
                    push(&mut phrases, (first, last), std::mem::take(&mut pending_events), previous_end);
                    previous_end = words[last].end;
                    pending_events = events;
                    current = Some((index, index));
                } else {
                    current = Some((first, index));
                }
            }
            None => current = Some((index, index)),
        }
    }
    if let Some(range) = current {
        push(&mut phrases, range, pending_events, previous_end);
    }
    phrases
}

/// Where a line is said again a little later: (earlier phrase index, later phrase index).
pub fn retakes(transcript: &Transcript, phrases: &[Phrase]) -> Vec<(usize, usize)> {
    // Normalised spoken tokens, without fillers, with their phrase.
    let mut tokens: Vec<(String, f64, usize)> = Vec::new();
    for (phrase_index, phrase) in phrases.iter().enumerate() {
        for word in &transcript.words[phrase.words.clone()] {
            if word.event || word.is_filler() {
                continue;
            }
            let token = normalise(&word.text);
            if !token.is_empty() {
                tokens.push((token, word.start, phrase_index));
            }
        }
    }
    let mut pairs = Vec::new();
    let mut p = 0;
    while p < tokens.len() {
        let mut best: Option<(usize, usize)> = None; // (q, length)
        let mut q = p + 1;
        while q < tokens.len() && tokens[q].1 - tokens[p].1 <= RETAKE_WINDOW {
            let mut length = 0;
            while q + length < tokens.len() && p + length < q && tokens[p + length].0 == tokens[q + length].0 {
                length += 1;
            }
            let chars: usize = tokens[p..p + length].iter().map(|token| token.0.chars().count()).sum();
            let counts = length >= 4 || (length >= 3 && chars >= 14) || (length >= 2 && chars >= 16);
            if counts && best.is_none_or(|(_, best_length)| length > best_length) {
                best = Some((q, length));
            }
            q += 1;
        }
        match best {
            Some((q, length)) => {
                let pair = (tokens[p].2, tokens[q].2);
                if pair.0 != pair.1 && !pairs.contains(&pair) {
                    pairs.push(pair);
                }
                p += length;
            }
            None => p += 1,
        }
    }
    pairs
}

fn normalise(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .map(|c| if c == 'ё' { 'е' } else { c })
        .collect()
}

/// Everything the phrase view says about one take.
pub struct TakeView {
    pub take: String,
    pub transcript: Transcript,
    pub phrases: Vec<Phrase>,
    /// For each phrase, the phrases that say its words again later.
    pub said_again: Vec<Vec<usize>>,
}

impl TakeView {
    pub fn new(take: &str, transcript: Transcript) -> Self {
        let phrases = phrases(take, &transcript);
        let mut said_again = vec![Vec::new(); phrases.len()];
        for (earlier, later) in retakes(&transcript, &phrases) {
            said_again[earlier].push(later);
        }
        Self { take: take.to_owned(), transcript, phrases, said_again }
    }

    pub fn phrase_index(&self, id: &str) -> Result<usize> {
        let id = id.trim();
        self.phrases.iter().position(|phrase| phrase.id.eq_ignore_ascii_case(id)).ok_or_else(|| {
            EditError::invalid(format!(
                "no phrase “{id}” in take {}: its phrases are {}1 to {}",
                self.take,
                self.take,
                self.phrases.last().map_or("none", |phrase| phrase.id.as_str())
            ))
        })
    }

    /// One phrase as a line: `A7  22.06-24.84  +4.2s [breath]  text  {hints}`.
    pub fn line(&self, index: usize, pause_note: Option<&str>) -> String {
        let phrase = &self.phrases[index];
        let words = &self.transcript.words;
        let mut line = format!("{:<6} {:>7.2}-{:<7.2}", phrase.id, phrase.start, phrase.end);
        if phrase.gap_before >= 1.0 {
            let _ = write!(line, " +{:.1}s", phrase.gap_before);
        }
        for event in &phrase.events_before {
            let _ = write!(line, " {}", words[*event].text);
        }
        if let Some(note) = pause_note {
            let _ = write!(line, " ({note})");
        }
        line.push_str("  ");
        let mut first = true;
        let mut cut_off = false;
        for word in &words[phrase.words.clone()] {
            if !first {
                line.push(' ');
            }
            first = false;
            if word.is_filler() {
                let _ = write!(line, "~{}~", word.text);
            } else {
                line.push_str(&word.text);
            }
            cut_off |= word.is_cut_off();
        }
        let mut hints = Vec::new();
        if cut_off {
            hints.push("false start".to_owned());
        }
        if !self.said_again[index].is_empty() {
            let later: Vec<&str> = self.said_again[index].iter().map(|i| self.phrases[*i].id.as_str()).collect();
            hints.push(format!("said again in {}", later.join(", ")));
        }
        if !hints.is_empty() {
            let _ = write!(line, "  {{{}}}", hints.join("; "));
        }
        line
    }

    /// The words of a phrase with their indices and times, for trimming inside it.
    pub fn word_lines(&self, index: usize) -> String {
        let phrase = &self.phrases[index];
        let mut out = format!("{} words:\n", phrase.id);
        for (n, word) in self.transcript.words[phrase.words.clone()].iter().enumerate() {
            let _ = writeln!(out, "  {n:>3} {:>7.2}-{:<7.2} {}", word.start, word.end, word.text);
        }
        out
    }

    /// Spoken text of phrases `range`, fillers dropped.
    pub fn text(&self, words: Range<usize>) -> String {
        let mut text = String::new();
        for word in &self.transcript.words[words] {
            if word.event || word.is_filler() {
                continue;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&word.text);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.into(), start, end, event: false, speaker: Some("s0".into()) }
    }

    fn event(text: &str, start: f64, end: f64) -> Word {
        Word { event: true, ..word(text, start, end) }
    }

    #[test]
    fn fillers_are_hesitations_not_short_words() {
        for filler in ["э-э-э,", "Э-э-э", "а-а-а", "м-м-м", "ммм", "эм", "um,", "Uh", "ээээ", "«эээ"]
        {
            assert!(is_filler(filler), "{filler}");
        }
        for word in ["а", "А,", "a", "и", "мама", "ааа-привет", "что-то", "Э-это", "ну", "чтооо"]
        {
            assert!(!is_filler(word), "{word}");
        }
    }

    #[test]
    fn cut_off_words_are_false_starts() {
        assert!(word("2025-м--", 0.0, 1.0).is_cut_off());
        assert!(word("ма-", 0.0, 1.0).is_cut_off());
        assert!(word("--", 0.0, 1.0).is_cut_off());
        assert!(!word("что-то", 0.0, 1.0).is_cut_off());
        assert!(!word("2025-м", 0.0, 1.0).is_cut_off());
    }

    #[test]
    fn reads_scribe_and_splits_phrases_on_pauses() {
        let json = br#"{"language_code":"rus","words":[
            {"text":"[breath]","start":0.1,"end":0.5,"type":"audio_event","speaker_id":"s0"},
            {"text":"Hello","start":0.6,"end":0.9,"type":"word","speaker_id":"s0"},
            {"text":" ","start":0.9,"end":1.0,"type":"spacing","speaker_id":"s0"},
            {"text":"there.","start":1.0,"end":1.3,"type":"word","speaker_id":"s0"},
            {"text":"[click]","start":1.5,"end":2.5,"type":"audio_event","speaker_id":"s0"},
            {"text":"Again","start":3.0,"end":3.4,"type":"word","speaker_id":"s0"}]}"#;
        let transcript = Transcript::from_scribe(json).unwrap();
        assert_eq!(transcript.language.as_deref(), Some("rus"));
        assert_eq!(transcript.words.len(), 5);
        let phrases = phrases("A", &transcript);
        assert_eq!(phrases.len(), 2);
        assert_eq!(phrases[0].id, "A1");
        assert_eq!(phrases[0].events_before, vec![0]);
        assert_eq!((phrases[0].start, phrases[0].end), (0.6, 1.3));
        assert_eq!(phrases[1].id, "A2");
        assert_eq!(phrases[1].events_before, vec![3]);
        assert!((phrases[1].gap_before - 1.7).abs() < 1e-9);
    }

    #[test]
    fn a_line_said_again_is_flagged_on_the_earlier_take() {
        let transcript = Transcript {
            language: None,
            words: vec![
                word("I", 0.0, 0.2),
                word("have", 0.2, 0.4),
                word("never", 0.4, 0.6),
                word("been", 0.6, 0.8),
                word("so--", 0.8, 1.0),
                event("[sigh]", 1.5, 2.0),
                word("I", 3.0, 3.2),
                word("have", 3.2, 3.4),
                word("~um~", 3.4, 3.5),
                word("never", 3.5, 3.7),
                word("been", 3.7, 3.9),
                word("so", 3.9, 4.1),
                word("wrong.", 4.1, 4.4),
            ],
        };
        let view = TakeView::new("A", transcript);
        assert_eq!(view.phrases.len(), 2);
        assert_eq!(view.said_again[0], vec![1]);
        assert!(view.said_again[1].is_empty());
        let line = view.line(0, None);
        assert!(line.contains("{false start; said again in A2}"), "{line}");
        assert!(view.line(1, None).contains("+2.0s [sigh]"), "{}", view.line(1, None));
    }
}
