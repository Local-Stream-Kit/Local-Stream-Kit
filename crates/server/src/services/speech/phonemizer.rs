//! Pronouncing words misaki's dictionary doesn't know (names, slang):
//! OpenPhonemizer (BSD-3-Clause-Clear), a small model trained to match
//! espeak-ng, with espeak's dictionary of 275,000 words. misaki calls it for
//! each unknown word (its `Fallback`); without it, such words were spelled
//! out letter by letter.
//!
//! The model (a DeepPhonemizer forward transformer) takes the word's letters,
//! each repeated `char_repeats` times between a start and an end token, and
//! gives, for each position, scores over its phoneme symbols; the best
//! symbol at each position, with repeats and special tokens left out, is the
//! pronunciation in espeak's US-English IPA, which [`to_kokoro`] turns into
//! Kokoro's symbols.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, anyhow};
use misaki_rs::Fallback;
use misaki_rs::fallback::FallbackError;
use serde::Deserialize;

use super::onnx::{Input, InputData, Model};

/// The model's companion file (`openphonemizer-en_us.json`).
#[derive(Deserialize)]
struct Data {
    char_repeats: usize,
    start_index: i64,
    end_index: i64,
    lowercase: bool,
    text_tokens: HashMap<String, i64>,
    phoneme_tokens: HashMap<String, String>,
    dictionary: HashMap<String, String>,
}

pub struct OpenPhonemizer {
    model: Mutex<Model>,
    letters: HashMap<char, i64>,
    /// By token id; special tokens are empty.
    symbols: Vec<String>,
    repeats: usize,
    start: i64,
    end: i64,
    lowercase: bool,
    dictionary: HashMap<String, String>,
}

impl OpenPhonemizer {
    pub fn load(runtime: &Path, model: &Path, data: &Path) -> anyhow::Result<OpenPhonemizer> {
        let text =
            std::fs::read_to_string(data).with_context(|| format!("couldn't read {}", data.display()))?;
        let data: Data =
            serde_json::from_str(&text).with_context(|| format!("{} is damaged", data.display()))?;
        let letters = data
            .text_tokens
            .iter()
            .filter_map(|(text, id)| {
                let mut chars = text.chars();
                let c = chars.next()?;
                chars.next().is_none().then_some((c, *id))
            })
            .collect();
        let count = data
            .phoneme_tokens
            .keys()
            .filter_map(|id| id.parse::<usize>().ok())
            .max()
            .map_or(0, |max| max + 1);
        let mut symbols = vec![String::new(); count];
        for (id, symbol) in &data.phoneme_tokens {
            let Ok(id) = id.parse::<usize>() else { continue };
            // Padding and the start / end tokens say nothing.
            if symbol != "_" && !symbol.starts_with('<') {
                symbols[id] = symbol.clone();
            }
        }
        let model = Model::load(runtime, model, 1)?;
        model.expect_inputs(&["text"])?;
        Ok(OpenPhonemizer {
            model: Mutex::new(model),
            letters,
            symbols,
            repeats: data.char_repeats.max(1),
            start: data.start_index,
            end: data.end_index,
            lowercase: data.lowercase,
            dictionary: data.dictionary,
        })
    }

    /// The word in espeak's US-English IPA.
    pub fn ipa(&self, word: &str) -> anyhow::Result<String> {
        let word = if self.lowercase { word.to_lowercase() } else { word.to_string() };
        if let Some(known) = self.dictionary.get(&word) {
            return Ok(known.clone());
        }
        let mut ids = vec![self.start];
        for c in word.chars() {
            if let Some(id) = self.letters.get(&c) {
                ids.extend(std::iter::repeat_n(*id, self.repeats));
            }
        }
        ids.push(self.end);
        if ids.len() <= 2 {
            return Err(anyhow!("no letters it knows"));
        }
        let length = ids.len();
        let scores = {
            let mut model = self.model.lock().map_err(|_| anyhow!("the phonemizer broke earlier"))?;
            model.run(&mut [Input {
                name: c"text",
                shape: &[1, length as i64],
                data: InputData::I64(&mut ids),
            }])?
        };
        let classes = scores.len() / length;
        if classes == 0 || classes * length != scores.len() {
            return Err(anyhow!("unexpected output size {}", scores.len()));
        }
        let mut out = String::new();
        let mut previous = None;
        for position in scores.chunks_exact(classes) {
            let best =
                position.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |(index, _)| index);
            if previous != Some(best) {
                if let Some(symbol) = self.symbols.get(best) {
                    out.push_str(symbol);
                }
                previous = Some(best);
            }
        }
        Ok(out)
    }
}

/// misaki's fallback for one accent, sharing the model.
pub struct PhonemizerFallback {
    pub phonemizer: Arc<OpenPhonemizer>,
    pub british: bool,
}

impl Fallback for PhonemizerFallback {
    fn phonemize(&self, word: &str) -> Result<String, FallbackError> {
        // Words without vowels ("gg", "brb") are said letter by letter.
        if !word.chars().any(|c| "aeiouyAEIOUY".contains(c)) {
            let letters: Vec<&str> =
                word.chars().filter_map(|c| letter_name(c.to_ascii_lowercase(), self.british)).collect();
            if !letters.is_empty() {
                return Ok(to_kokoro(&letters.join(" "), self.british));
            }
        }
        let ipa = self.phonemizer.ipa(word).map_err(|error| {
            tracing::debug!(word, %error, "couldn't guess how a word is said");
            FallbackError::NoPhonemes { word: word.to_string() }
        })?;
        let phonemes = to_kokoro(&ipa, self.british);
        if phonemes.is_empty() {
            return Err(FallbackError::NoPhonemes { word: word.to_string() });
        }
        Ok(phonemes)
    }
}

/// A letter's name, in espeak's IPA.
fn letter_name(letter: char, british: bool) -> Option<&'static str> {
    Some(match letter {
        'a' => "ˈeɪ",
        'b' => "bˈiː",
        'c' => "sˈiː",
        'd' => "dˈiː",
        'e' => "ˈiː",
        'f' => "ˈɛf",
        'g' => "dʒˈiː",
        'h' => "ˈeɪtʃ",
        'i' => "ˈaɪ",
        'j' => "dʒˈeɪ",
        'k' => "kˈeɪ",
        'l' => "ˈɛl",
        'm' => "ˈɛm",
        'n' => "ˈɛn",
        'o' => "ˈoʊ",
        'p' => "pˈiː",
        'q' => "kjˈuː",
        'r' => "ˈɑːɹ",
        's' => "ˈɛs",
        't' => "tˈiː",
        'u' => "jˈuː",
        'v' => "vˈiː",
        'w' => "dˈʌbəljuː",
        'x' => "ˈɛks",
        'y' => "wˈaɪ",
        'z' if british => "zˈɛd",
        'z' => "zˈiː",
        _ => return None,
    })
}

/// espeak's IPA as Kokoro's symbols: the mapping misaki (Python) uses for its
/// espeak fallback.
pub fn to_kokoro(ipa: &str, british: bool) -> String {
    let mut s: String =
        ipa.chars().filter(|c| !matches!(c, '\u{361}' | '\u{303}' | '\u{32F}' | '\u{325}')).collect();
    // Syllabic consonants: "n̩" → "ᵊn".
    let mut syllabic = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(chars.peek(), Some('\u{329}' | '\u{30D}')) {
            chars.next();
            syllabic.push('ᵊ');
        }
        syllabic.push(c);
    }
    s = syllabic;
    let replace =
        |s: String, pairs: &[(&str, &str)]| pairs.iter().fold(s, |s, (from, to)| s.replace(from, to));
    s = replace(s, &[("aɪ", "I"), ("aʊ", "W"), ("eɪ", "A"), ("ɔɪ", "Y"), ("dʒ", "ʤ"), ("tʃ", "ʧ")]);
    s = if british {
        replace(s, &[("eə", "ɛː"), ("iə", "ɪə"), ("oʊ", "Q"), ("əʊ", "Q")])
    } else {
        replace(s, &[("oʊ", "O"), ("ɜːɹ", "ɜɹ"), ("ɜː", "ɜɹ"), ("ɪə", "iə")])
    };
    s = replace(
        s,
        &[
            ("ɚ", "əɹ"),
            ("ɝ", "ɜɹ"),
            ("r", "ɹ"),
            ("x", "k"),
            ("ç", "k"),
            ("ɐ", "ə"),
            ("ʔ", "t"),
            ("e", "A"),
            ("o", "ɔ"),
            ("'", ""),
        ],
    );
    if !british {
        s = s.replace('ː', "");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn espeak_ipa_becomes_kokoro_symbols() {
        assert_eq!(to_kokoro("həlˈoʊ", false), "həlˈO");
        assert_eq!(to_kokoro("həlˈoʊ", true), "həlˈQ");
        assert_eq!(to_kokoro("mˈɑːɹtɪn", false), "mˈɑɹtɪn");
        assert_eq!(to_kokoro("skˈɛdʒuːl", false), "skˈɛʤul");
        assert_eq!(to_kokoro("bˈɜːd", false), "bˈɜɹd");
        assert_eq!(to_kokoro("bˈʌʔn̩", false), "bˈʌtᵊn");
        assert_eq!(to_kokoro("tʃˈeɪn", true), "ʧˈAn");
        let gg: Vec<&str> = "gg".chars().filter_map(|c| letter_name(c, false)).collect();
        assert_eq!(to_kokoro(&gg.join(" "), false), "ʤˈi ʤˈi");
    }
}
