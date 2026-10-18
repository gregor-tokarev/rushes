//! Word-level transcripts and the phrase view the agent reads.
//!
//! The agent never types timestamps for speech: it reads phrases with ids (`A12`),
//! picks which to keep, and the cut plan turns ids into cut times. Hints (false
//! starts, lines said again a bit later, fillers, restarts) are computed here so the
//! agent does not have to spot them by reading.

use std::fmt::Write as _;
use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::error::{EditError, Result};

/// A silence at least this long between two spoken words starts a new phrase.
pub const PHRASE_GAP: f64 = 0.5;
/// How far ahead (seconds) a line said again counts as a retake.
const RETAKE_WINDOW: f64 = 120.0;
/// Most words of an abandoned attempt compared with its restart.
const RESTART_WORDS: usize = 5;
/// Longest pause before a restart, and longest time from an attempt's start to its
/// restart's, in seconds.
const RESTART_PAUSE: f64 = 6.0;
const RESTART_WINDOW: f64 = 12.0;
/// Words said twice in a row on purpose (“very, very”), not restarted.
const EMPHATIC: [&str; 31] = [
    "очень",
    "гораздо",
    "намного",
    "сильно",
    "слишком",
    "совсем",
    "много",
    "долго",
    "давно",
    "никогда",
    "всегда",
    "нет",
    "да",
    "так",
    "ну",
    "very",
    "really",
    "so",
    "much",
    "many",
    "more",
    "too",
    "far",
    "way",
    "no",
    "yes",
    "never",
    "always",
    "again",
    "ever",
    "bye",
];
/// What a speaker says between an attempt and its restart (“…, ну, …”, “…, I mean, …”).
const LINKING: [&str; 11] = ["ну", "вот", "как", "бы", "типа", "короче", "значит", "well", "like", "mean", "know"];

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

/// An attempt the speaker abandons and starts again at once (“I was go-- I was going”,
/// “прикольный, э-э-э, прикольный”, “стану-- станет”): inside a segment that keeps the
/// restart, the attempt is cut like a filler.
#[derive(Clone, Debug, PartialEq)]
pub struct Restart {
    /// From the attempt's first word up to the restart: indices into [`Transcript::words`].
    pub dropped: Range<usize>,
    /// The restart's first word.
    pub restart: usize,
}

/// The restarts of a take, in time order. An attempt is the same words said again (up
/// to [`RESTART_WORDS`]; its last word may be cut off, `стану--` before `станет`) after
/// nothing, a few linking words, or an attempt broken off mid-word; or a lone word cut
/// off mid-way before the speaker goes on. Words said twice on purpose ([`EMPHATIC`]) and
/// a word ending one sentence and starting the next are not restarts.
pub fn restarts(transcript: &Transcript) -> Vec<Restart> {
    let words = &transcript.words;
    let mut tokens: Vec<Token> = Vec::new();
    for (index, word) in words.iter().enumerate() {
        if word.event || word.is_filler() {
            continue;
        }
        let text = normalise(&word.text);
        if text.is_empty() {
            // A lone `--` after a word: that word broke off.
            if word.is_cut_off()
                && let Some(last) = tokens.last_mut()
            {
                last.cut_off = true;
            }
            continue;
        }
        tokens.push(Token {
            index,
            text,
            start: word.start,
            end: word.end,
            cut_off: word.is_cut_off(),
            speaker: word.speaker.clone(),
        });
    }
    let mut out = Vec::new();
    let mut p = 0;
    while p < tokens.len() {
        if let Some(q) = restart_after(words, &tokens, p) {
            out.push(Restart { dropped: tokens[p].index..tokens[q].index, restart: tokens[q].index });
            // The restart can be abandoned in turn (“мне, мне, мне”).
            p = q;
            continue;
        }
        if let Some(next) = tokens.get(p + 1)
            && tokens[p].cut_off
            && next.start - tokens[p].end <= RESTART_PAUSE
        {
            out.push(Restart { dropped: tokens[p].index..next.index, restart: next.index });
        }
        p += 1;
    }
    out
}

struct Token {
    index: usize,
    text: String,
    start: f64,
    end: f64,
    cut_off: bool,
    speaker: Option<String>,
}

/// Where the attempt starting at token `p` is said again, if it is.
fn restart_after(words: &[Word], tokens: &[Token], p: usize) -> Option<usize> {
    for length in (1..=RESTART_WORDS).rev() {
        for gap in 0..=8 {
            let q = p + length + gap;
            if q + length > tokens.len() || tokens[q].start - tokens[p].start > RESTART_WINDOW {
                break;
            }
            if tokens[q].start - tokens[q - 1].end > RESTART_PAUSE || tokens[q].speaker != tokens[p].speaker {
                continue;
            }
            let between = &tokens[p + length..q];
            let linking = gap <= 2 && between.iter().all(|token| LINKING.contains(&token.text.as_str()));
            let broke_off = between.last().is_some_and(|token| token.cut_off) && (length >= 2 || gap <= 3);
            if gap > 0 && !linking && !broke_off {
                continue;
            }
            let (first, second) = (&tokens[p..p + length], &tokens[q..q + length]);
            let (a, b) = (&first[length - 1], &second[length - 1]);
            let same = first.iter().zip(second).take(length - 1).all(|(a, b)| a.text == b.text)
                && (a.text == b.text || (a.cut_off && b.text.starts_with(&a.text)));
            if !same || (length == 1 && gap == 0 && EMPHATIC.contains(&a.text.as_str())) {
                continue;
            }
            // “…for JavaScript. JavaScript, I think…”: a sentence ended, nothing was abandoned.
            let before = words[tokens[q - 1].index].text.trim_end_matches(['"', '»', ')', '\'']);
            if length == 1 && before.ends_with(['.', '!', '?']) && !before.ends_with("...") {
                continue;
            }
            return Some(q);
        }
    }
    None
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
    pub restarts: Vec<Restart>,
    /// For each word, whether it belongs to an abandoned attempt.
    abandoned: Vec<bool>,
}

impl TakeView {
    pub fn new(take: &str, transcript: Transcript) -> Self {
        let phrases = phrases(take, &transcript);
        let mut said_again = vec![Vec::new(); phrases.len()];
        for (earlier, later) in retakes(&transcript, &phrases) {
            said_again[earlier].push(later);
        }
        let restarts = restarts(&transcript);
        let mut abandoned = vec![false; transcript.words.len()];
        for restart in &restarts {
            abandoned[restart.dropped.clone()].fill(true);
        }
        Self { take: take.to_owned(), transcript, phrases, said_again, restarts, abandoned }
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
        let mut cut_off = false;
        for index in phrase.words.clone() {
            let word = &words[index];
            if index > phrase.words.start {
                line.push(' ');
            }
            // An abandoned attempt is marked as one run: `~I was go--~ I was going`.
            let abandoned = self.abandoned[index];
            if abandoned && (index == phrase.words.start || !self.abandoned[index - 1]) {
                line.push('~');
            }
            if word.is_filler() && !abandoned {
                let _ = write!(line, "~{}~", word.text);
            } else {
                line.push_str(&word.text);
            }
            if abandoned && (index + 1 == phrase.words.end || !self.abandoned[index + 1]) {
                line.push('~');
            }
            cut_off |= word.is_cut_off() && !abandoned;
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
        // The broken-off attempt is a restart of A2's start: marked, and no longer a false start to handle.
        assert!(line.contains("~I have never been so--~  {said again in A2}"), "{line}");
        assert_eq!(view.restarts, vec![Restart { dropped: 0..6, restart: 6 }]);
        assert!(view.line(1, None).contains("+2.0s [sigh]"), "{}", view.line(1, None));
    }

    /// Words every 0.3 s from `start`.
    fn said(text: &str, start: f64) -> Vec<Word> {
        text.split_whitespace()
            .enumerate()
            .map(|(n, text)| word(text, start + n as f64 * 0.3, start + n as f64 * 0.3 + 0.25))
            .collect()
    }

    /// The abandoned words of `text`, or what it keeps.
    fn cut(text: &str) -> String {
        let transcript = Transcript { language: None, words: said(text, 0.0) };
        let restarts = restarts(&transcript);
        let gone: Vec<usize> = restarts.iter().flat_map(|restart| restart.dropped.clone()).collect();
        let kept: Vec<&str> = transcript
            .words
            .iter()
            .enumerate()
            .filter(|(index, word)| !gone.contains(index) && !word.is_filler())
            .map(|(_, word)| word.text.as_str())
            .collect();
        kept.join(" ")
    }

    #[test]
    fn restarts_lose_the_abandoned_attempt() {
        // From a real recording: repeats, a broken-off word, a restart after linking words.
        assert_eq!(cut("А, еще один прикольный, э-э-э, прикольный use case"), "А, еще один прикольный use case");
        assert_eq!(cut("Это месяцы, а может быть, может быть, это год"), "Это месяцы, а может быть, это год");
        assert_eq!(cut("Мне, мне, мне лень считать."), "мне лень считать.");
        assert_eq!(cut("порог входа стану-- станет настолько низким"), "порог входа станет настолько низким");
        assert_eq!(cut("насколько долго два -- две этих вершины"), "насколько долго две этих вершины");
        assert_eq!(cut("И что мне как бы, что мне больше всего нравится?"), "И что мне больше всего нравится?");
        assert_eq!(cut("то, наверное, ну, наверное, я не хочу"), "то, наверное, я не хочу");
        assert_eq!(
            cut("Они патчат систему, на которой они-- они патчат систему, на которой исполняют код"),
            "они патчат систему, на которой исполняют код"
        );
        assert_eq!(cut("I was go-- I was going to say"), "I was going to say");
        // Said twice on purpose, or a sentence that ends: kept as said.
        for text in [
            "вам дали очень, очень активного мидла",
            "гораздо, гораздо дешевле",
            "сказать: То-то и то-то сделано",
            "коммуникация с заказчиками, коммуникация с коллегами",
            "Не уволят — сократят зарплаты",
            "это runtime для JavaScript. JavaScript, я думаю, все знают",
            "Да, да, заменит он меня",
        ] {
            assert_eq!(cut(text), text);
        }
    }

    #[test]
    fn a_restart_after_a_long_pause_or_from_another_speaker_is_not_one() {
        let mut words = said("Они,", 0.0);
        words.extend(said("они способны", 4.0));
        let transcript = Transcript { language: None, words: words.clone() };
        assert_eq!(restarts(&transcript).len(), 1, "a pause of a few seconds before the restart");
        words.iter_mut().skip(1).for_each(|word| word.start += 10.0);
        words.iter_mut().skip(1).for_each(|word| word.end += 10.0);
        assert!(restarts(&Transcript { language: None, words: words.clone() }).is_empty());
        let mut words = said("I think I think so", 0.0);
        words[2].speaker = Some("s1".into());
        words[3].speaker = Some("s1".into());
        words[4].speaker = Some("s1".into());
        assert!(restarts(&Transcript { language: None, words }).is_empty());
    }
}
