//! What's read aloud: the text after the speech settings' few rules (say
//! instead, repeats once, links left out, a length limit), with names split so the
//! dictionary knows their parts ("TestViewer" → "Test Viewer"), and words in
//! capitals lowercased: misaki reads short ones as abbreviations, letter by
//! letter ("LETS GO" → "L E T S go"), while real abbreviations (USA, TV)
//! are in the dictionary in lowercase too.

use protocol::SpeechSettings;
use protocol::links::is_link;

/// The text to read, or empty for nothing.
pub fn prepare(text: &str, settings: &SpeechSettings) -> String {
    let mut words: Vec<String> = text
        .split_whitespace()
        .filter(|word| !(settings.skip_links && is_link(word)))
        .map(|word| say_instead(word, settings))
        .collect();
    if settings.collapse_repeats {
        words = collapse_repeats(words);
    }
    let text = words.iter().map(|word| unshout(&split_name(word))).collect::<Vec<_>>().join(" ");
    shorten(&text, settings.max_chars as usize)
}

/// Seconds of silence added for each slash after the first. One slash is
/// just the split: each piece is read on its own, and Kokoro's silence at
/// the end of one and the start of the next is about 0.75 s.
const PAUSE_PER_SLASH: f32 = 0.5;
/// The longest silence added, in seconds.
const MAX_PAUSE: f32 = 3.0;

/// The text in pieces to say, each with the silence to add before it in
/// seconds.
/// Slashes standing alone ("/ / /", "///") are a pause, longer with more
/// slashes, as in other streaming tools' text to speech: Kokoro leaves
/// slashes out, and pauses only a moment for punctuation. Pauses at the
/// start or end are left out, as is a slash in a word ("and/or").
pub fn pieces(text: &str) -> Vec<(f32, String)> {
    let mut pieces: Vec<(f32, String)> = Vec::new();
    let mut slashes = 0;
    for word in text.split_whitespace() {
        if word.chars().all(|c| c == '/') {
            slashes += word.len();
            continue;
        }
        match pieces.last_mut() {
            Some((_, piece)) if slashes == 0 => {
                piece.push(' ');
                piece.push_str(word);
            }
            last => {
                let pause = if last.is_some() {
                    ((slashes - 1) as f32 * PAUSE_PER_SLASH).min(MAX_PAUSE)
                } else {
                    0.0
                };
                pieces.push((pause, word.to_string()));
            }
        }
        slashes = 0;
    }
    pieces
}

/// Runs of the same word (ignoring case and punctuation) as one, ending
/// like the run's last word: "KEKW KEKW KEKW." → "KEKW.".
fn collapse_repeats(words: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    for word in words {
        match out.last_mut() {
            Some(previous) if !bare(&word).is_empty() && bare(&word).eq_ignore_ascii_case(bare(previous)) => {
                let ending = &word[word.rfind(bare(&word)).unwrap_or(0) + bare(&word).len()..];
                let kept = bare(previous).to_string();
                let start = previous.find(&kept).unwrap_or(0);
                *previous = format!("{}{kept}{ending}", &previous[..start]);
            }
            _ => out.push(word),
        }
    }
    out
}

/// A word without the punctuation around it.
fn bare(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_alphanumeric())
}

/// The word, or what to say instead (keeping its punctuation). Tried as it
/// is, then without the punctuation around it, then as letters and digits
/// only, so entries with symbols ("<3", ":)") match too.
fn say_instead(word: &str, settings: &SpeechSettings) -> String {
    let unquoted = word
        .trim_end_matches(|c: char| ".,!?;:\"')".contains(c))
        .trim_start_matches(|c: char| "\"'(".contains(c));
    for core in [word, unquoted, bare(word)] {
        if core.is_empty() {
            continue;
        }
        if let Some(entry) =
            settings.pronunciations.iter().find(|entry| entry.word.trim().eq_ignore_ascii_case(core))
        {
            return word.replacen(core, entry.say.trim(), 1);
        }
    }
    word.to_string()
}

/// Splits a name at a lower-to-upper change, between letters and digits, and
/// at underscores: "xX_Sniper99" → "xX Sniper 99". Words in capitals
/// ("KEKW") stay as they are.
fn split_name(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 4);
    let mut previous: Option<char> = None;
    for c in word.chars() {
        if c == '_' {
            out.push(' ');
            previous = None;
            continue;
        }
        if let Some(before) = previous {
            let case_change = before.is_lowercase() && c.is_uppercase();
            let digit_change =
                before.is_alphabetic() && c.is_ascii_digit() || before.is_ascii_digit() && c.is_alphabetic();
            if case_change || digit_change {
                out.push(' ');
            }
        }
        out.push(c);
        previous = Some(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Words in capitals (two letters or more) in lowercase: "LETS GO!" →
/// "lets go!". Other words keep their case.
fn unshout(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let letters = word.chars().filter(|c| c.is_alphabetic()).count();
            if letters >= 2 && !word.chars().any(char::is_lowercase) {
                word.to_lowercase()
            } else {
                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// At most `max` characters (0 = no limit), cut after a whole word.
fn shorten(text: &str, max: usize) -> String {
    if max == 0 || text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    match cut.rfind(char::is_whitespace) {
        Some(end) if end > 0 => cut[..end].trim_end().to_string(),
        _ => cut,
    }
}

#[cfg(test)]
mod tests {
    use protocol::speech::Pronunciation;

    use super::*;

    fn settings() -> SpeechSettings {
        SpeechSettings { max_chars: 0, ..SpeechSettings::default() }
    }

    #[test]
    fn names_are_split_for_the_dictionary() {
        assert_eq!(prepare("TestViewer says hi", &settings()), "Test Viewer says hi");
        assert_eq!(prepare("xX_Sniper99_Xx", &settings()), "x X Sniper 99 Xx");
        assert_eq!(prepare("KEKW 500bits", &settings()), "kekw 500 bits");
    }

    #[test]
    fn capitals_are_read_as_words() {
        assert_eq!(prepare("LETS GO, THIS IS AMAZING!", &settings()), "lets go, this is amazing!");
        assert_eq!(prepare("I said A, not PogU", &settings()), "I said A, not Pog U");
        assert_eq!(prepare("GG4U", &settings()), "gg 4 U");
    }

    #[test]
    fn repeats_are_read_once_when_asked() {
        let text = "KEKW KEKW kekw KEKW! that was so so funny";
        assert_eq!(prepare(text, &settings()), "kekw! that was so funny");
        assert_eq!(prepare("(lol lol lol)", &settings()), "(lol)");
        let all = SpeechSettings { collapse_repeats: false, ..settings() };
        assert_eq!(prepare(text, &all), "kekw kekw kekw kekw! that was so so funny");
    }

    #[test]
    fn long_texts_are_cut_at_a_word() {
        let short = SpeechSettings { max_chars: 12, ..settings() };
        assert_eq!(prepare("hello there my friend", &short), "hello there");
        assert_eq!(prepare("supercalifragilistic", &short), "supercalifra");
        assert_eq!(prepare("hi", &short), "hi");
    }

    #[test]
    fn words_can_be_said_differently() {
        let mut settings = settings();
        settings.pronunciations.push(Pronunciation { word: "Marrtin".into(), say: "Martin".into() });
        assert_eq!(prepare("thanks marrtin!", &settings), "thanks Martin!");
    }

    #[test]
    fn links_are_left_out_when_asked() {
        let said = |text| prepare(text, &settings());
        assert_eq!(said("check https://example.com/a?b=1 out!"), "check out!");
        assert_eq!(said("follow twitch.tv/marrtin and www.Example.org."), "follow and");
        assert_eq!(said("my site (example.com) is up"), "my site is up");
        assert_eq!(said("youtu.be/abc123"), "");
        // Not links.
        assert_eq!(said("e.g. 1.5 times, ok.then"), "e.g. 1.5 times, ok.then");
        let kept = SpeechSettings { skip_links: false, ..settings() };
        assert_eq!(prepare("see example.com", &kept), "see example.com");
    }

    #[test]
    fn slashes_are_pauses() {
        let said = |text| pieces(&prepare(text, &settings()));
        assert_eq!(said("hello there"), [(0.0, "hello there".to_string())]);
        assert_eq!(said("hi / there"), [(0.0, "hi".to_string()), (0.0, "there".to_string())]);
        assert_eq!(said("hi / / / there"), [(0.0, "hi".to_string()), (1.0, "there".to_string())]);
        assert_eq!(said("hi // there /"), [(0.0, "hi".to_string()), (0.5, "there".to_string())]);
        assert_eq!(said("/ hi ////////// there"), [(0.0, "hi".to_string()), (3.0, "there".to_string())]);
        assert_eq!(said("this and/or that"), [(0.0, "this and/or that".to_string())]);
        assert_eq!(said("/ / /"), []);
    }

    #[test]
    fn symbols_can_be_said_differently() {
        let mut settings = settings();
        settings.pronunciations.push(Pronunciation { word: "<3".into(), say: "heart".into() });
        settings.pronunciations.push(Pronunciation { word: ":)".into(), say: "smile".into() });
        assert_eq!(prepare("love you <3", &settings), "love you heart");
        assert_eq!(prepare("love you <3!", &settings), "love you heart!");
        assert_eq!(prepare("hi :) 3 times", &settings), "hi smile 3 times");
    }
}
