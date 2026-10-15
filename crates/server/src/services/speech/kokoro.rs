//! Kokoro v1.0 (82M, Apache-2.0) through ONNX Runtime: text → phonemes
//! (misaki-rs, US or British by the voice) → tokens → audio at 24 kHz.
//!
//! The model takes `input_ids` (int64, `[1, n + 2]`: the tokens with a 0 at
//! each end, at most 510 tokens), `style` (float32 `[1, 256]`: the voice's
//! row for that many tokens) and `speed` (float32 `[1]`), and gives the
//! waveform.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use misaki_rs::{G2P, Language};

use super::onnx::{Input, InputData, Model};
use super::phonemizer::{OpenPhonemizer, PhonemizerFallback};

pub const SAMPLE_RATE: u32 = 24_000;
/// Tokens per model call (the context is 512 with the two pads).
const MAX_TOKENS: usize = 510;
const STYLE: usize = 256;

/// Kokoro's phoneme vocabulary (its `config.json`), as `(symbol, id)`.
const VOCAB: &[(char, i64)] = &[
    (';', 1),
    (':', 2),
    (',', 3),
    ('.', 4),
    ('!', 5),
    ('?', 6),
    ('—', 9),
    ('…', 10),
    ('"', 11),
    ('(', 12),
    (')', 13),
    ('“', 14),
    ('”', 15),
    (' ', 16),
    ('\u{303}', 17),
    ('ʣ', 18),
    ('ʥ', 19),
    ('ʦ', 20),
    ('ʨ', 21),
    ('ᵝ', 22),
    ('\u{AB67}', 23),
    ('A', 24),
    ('I', 25),
    ('O', 31),
    ('Q', 33),
    ('S', 35),
    ('T', 36),
    ('W', 39),
    ('Y', 41),
    ('ᵊ', 42),
    ('a', 43),
    ('b', 44),
    ('c', 45),
    ('d', 46),
    ('e', 47),
    ('f', 48),
    ('h', 50),
    ('i', 51),
    ('j', 52),
    ('k', 53),
    ('l', 54),
    ('m', 55),
    ('n', 56),
    ('o', 57),
    ('p', 58),
    ('q', 59),
    ('r', 60),
    ('s', 61),
    ('t', 62),
    ('u', 63),
    ('v', 64),
    ('w', 65),
    ('x', 66),
    ('y', 67),
    ('z', 68),
    ('ɑ', 69),
    ('ɐ', 70),
    ('ɒ', 71),
    ('æ', 72),
    ('β', 75),
    ('ɔ', 76),
    ('ɕ', 77),
    ('ç', 78),
    ('ɖ', 80),
    ('ð', 81),
    ('ʤ', 82),
    ('ə', 83),
    ('ɚ', 85),
    ('ɛ', 86),
    ('ɜ', 87),
    ('ɟ', 90),
    ('ɡ', 92),
    ('ɥ', 99),
    ('ɨ', 101),
    ('ɪ', 102),
    ('ʝ', 103),
    ('ɯ', 110),
    ('ɰ', 111),
    ('ŋ', 112),
    ('ɳ', 113),
    ('ɲ', 114),
    ('ɴ', 115),
    ('ø', 116),
    ('ɸ', 118),
    ('θ', 119),
    ('œ', 120),
    ('ɹ', 123),
    ('ɾ', 125),
    ('ɻ', 126),
    ('ʁ', 128),
    ('ɽ', 129),
    ('ʂ', 130),
    ('ʃ', 131),
    ('ʈ', 132),
    ('ʧ', 133),
    ('ʊ', 135),
    ('ʋ', 136),
    ('ʌ', 138),
    ('ɣ', 139),
    ('ɤ', 140),
    ('χ', 142),
    ('ʎ', 143),
    ('ʒ', 147),
    ('ʔ', 148),
    ('ˈ', 156),
    ('ˌ', 157),
    ('ː', 158),
    ('ʰ', 162),
    ('ʲ', 164),
    ('↓', 169),
    ('→', 171),
    ('↗', 172),
    ('↘', 173),
    ('ᵻ', 177),
];

/// Token ids where a long text may be split: punctuation, then spaces.
const SENTENCE_ENDS: [i64; 5] = [4, 5, 6, 9, 10];
const PAUSES: [i64; 3] = [1, 2, 3];
const SPACE: i64 = 16;

/// misaki-rs writes some sounds as two symbols joined by a zero-width
/// joiner; Kokoro has one symbol for each.
const JOINED: [(&str, &str); 9] = [
    ("a\u{200D}ɪ", "I"),
    ("a\u{200D}ʊ", "W"),
    ("e\u{200D}ɪ", "A"),
    ("o\u{200D}ʊ", "O"),
    ("ə\u{200D}ʊ", "Q"),
    ("ɔ\u{200D}ɪ", "Y"),
    ("d\u{200D}ʒ", "ʤ"),
    ("t\u{200D}ʃ", "ʧ"),
    ("e\u{200D}ə", "ɛː"),
];

/// The phonemes as Kokoro's symbols: joined sounds as one symbol, unknown
/// markers left out.
pub fn kokoro_phonemes(phonemes: &str) -> String {
    let mut out = phonemes.to_string();
    for (joined, single) in JOINED {
        out = out.replace(joined, single);
    }
    out.chars().filter(|c| *c != '\u{200D}' && *c != '❓').collect()
}

/// The phonemes as token ids; symbols Kokoro doesn't know are skipped.
pub fn tokens(phonemes: &str) -> Vec<i64> {
    let ids: HashMap<char, i64> = VOCAB.iter().copied().collect();
    let mut tokens: Vec<i64> = Vec::new();
    for c in phonemes.chars() {
        if let Some(id) = ids.get(&c) {
            // Runs of spaces read as one.
            if !(*id == SPACE && tokens.last() == Some(&SPACE)) {
                tokens.push(*id);
            }
        }
    }
    while tokens.first() == Some(&SPACE) {
        tokens.remove(0);
    }
    while tokens.last() == Some(&SPACE) {
        tokens.pop();
    }
    tokens
}

/// The tokens in pieces of at most `max`, split after a sentence where
/// possible, else after a pause, else at a space.
pub fn chunks(tokens: &[i64], max: usize) -> Vec<&[i64]> {
    let mut pieces = Vec::new();
    let mut rest = tokens;
    while rest.len() > max {
        let window = &rest[..max];
        let split_after = |ends: &[i64]| window.iter().rposition(|id| ends.contains(id)).filter(|at| *at > 0);
        let at = split_after(&SENTENCE_ENDS)
            .or_else(|| split_after(&PAUSES))
            .or_else(|| split_after(&[SPACE]))
            .map_or(max, |at| at + 1);
        let (piece, after) = rest.split_at(at);
        pieces.push(piece);
        rest = after;
        while rest.first() == Some(&SPACE) {
            rest = &rest[1..];
        }
    }
    if !rest.is_empty() {
        pieces.push(rest);
    }
    pieces
}

/// The files Kokoro needs, in its folder.
pub struct Files {
    pub runtime: PathBuf,
    pub model: PathBuf,
    pub voices: PathBuf,
    /// OpenPhonemizer, for words misaki doesn't know.
    pub phonemizer: PathBuf,
    pub phonemizer_data: PathBuf,
}

impl Files {
    pub fn in_dir(dir: &Path) -> Files {
        Files {
            runtime: dir.join(runtime_name()),
            model: dir.join("model.onnx"),
            voices: dir.join("voices"),
            phonemizer: dir.join("openphonemizer.onnx"),
            phonemizer_data: dir.join("openphonemizer.json"),
        }
    }

    pub fn voice(&self, id: &str) -> PathBuf {
        self.voices.join(format!("{id}.bin"))
    }
}

/// ONNX Runtime's library file name on this system.
pub fn runtime_name() -> &'static str {
    if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

pub struct Kokoro {
    model: Model,
    files: Files,
    voices: HashMap<String, Vec<f32>>,
    us: G2P,
    gb: G2P,
}

impl Kokoro {
    pub fn load(files: Files) -> anyhow::Result<Kokoro> {
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4);
        let model = Model::load(&files.runtime, &files.model, threads)?;
        model.expect_inputs(&["input_ids", "style", "speed"])?;
        // Words misaki doesn't know are guessed rather than spelled out.
        let phonemizer = std::sync::Arc::new(OpenPhonemizer::load(
            &files.runtime,
            &files.phonemizer,
            &files.phonemizer_data,
        )?);
        let g2p = |language, british| {
            let fallback = PhonemizerFallback { phonemizer: std::sync::Arc::clone(&phonemizer), british };
            G2P::with_fallback(language, Some(Box::new(fallback)))
        };
        Ok(Kokoro {
            model,
            files,
            voices: HashMap::new(),
            us: g2p(Language::EnglishUS, false),
            gb: g2p(Language::EnglishGB, true),
        })
    }

    /// A voice's style rows (`[n, 1, 256]` floats), loaded on first use.
    fn voice(&mut self, id: &str) -> anyhow::Result<&[f32]> {
        if !self.voices.contains_key(id) {
            let path = self.files.voice(id);
            let bytes = std::fs::read(&path).with_context(|| format!("no voice file {}", path.display()))?;
            if bytes.len() < STYLE * 4 || bytes.len() % (STYLE * 4) != 0 {
                return Err(anyhow!("the voice file {} is damaged", path.display()));
            }
            let floats = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect::<Vec<_>>();
            self.voices.insert(id.to_string(), floats);
        }
        Ok(&self.voices[id])
    }

    /// The text as Kokoro's symbols, in the voice's accent.
    pub fn phonemes(&self, text: &str, voice: &str) -> anyhow::Result<String> {
        let g2p = if voice.starts_with('b') { &self.gb } else { &self.us };
        let (phonemes, _) =
            g2p.g2p(text).map_err(|error| anyhow!("couldn't phonemize the text: {error:?}"))?;
        Ok(kokoro_phonemes(&phonemes))
    }

    /// The text spoken in a voice, as samples at [`SAMPLE_RATE`]. Slashes
    /// standing alone are pauses (see [`super::text::pieces`]).
    pub fn speak(&mut self, text: &str, voice: &str, speed: f32) -> anyhow::Result<Vec<f32>> {
        let mut audio = Vec::new();
        for (pause, piece) in super::text::pieces(text) {
            if !audio.is_empty() {
                audio.resize(audio.len() + (pause * SAMPLE_RATE as f32) as usize, 0.0);
            }
            self.speak_piece(&piece, voice, speed, &mut audio)?;
        }
        Ok(audio)
    }

    /// Text without pauses, added to `audio`.
    fn speak_piece(
        &mut self,
        text: &str,
        voice: &str,
        speed: f32,
        audio: &mut Vec<f32>,
    ) -> anyhow::Result<()> {
        let tokens = tokens(&self.phonemes(text, voice)?);
        let speed = if speed.is_finite() { speed.clamp(0.5, 2.0) } else { 1.0 };
        for piece in chunks(&tokens, MAX_TOKENS) {
            let rows = self.voice(voice)?;
            let row = piece.len().min(rows.len() / STYLE - 1);
            let mut style = rows[row * STYLE..(row + 1) * STYLE].to_vec();
            let mut ids = Vec::with_capacity(piece.len() + 2);
            ids.push(0);
            ids.extend_from_slice(piece);
            ids.push(0);
            let mut speed = [speed];
            let samples = self.model.run(&mut [
                Input { name: c"input_ids", shape: &[1, ids.len() as i64], data: InputData::I64(&mut ids) },
                Input { name: c"style", shape: &[1, STYLE as i64], data: InputData::F32(&mut style) },
                Input { name: c"speed", shape: &[1], data: InputData::F32(&mut speed) },
            ])?;
            audio.extend_from_slice(&samples);
        }
        Ok(())
    }
}

/// 16-bit mono PCM WAV.
pub fn wav(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misaki_symbols_become_kokoro_tokens() {
        // "Hello, George" as misaki-rs writes it (US).
        let phonemes = kokoro_phonemes("həlˈo\u{200D}ʊ , d\u{200D}ʒˈɔɹʤ ❓");
        assert_eq!(phonemes, "həlˈO , ʤˈɔɹʤ ");
        assert_eq!(tokens(&phonemes), [50, 83, 54, 156, 31, 16, 3, 16, 82, 156, 76, 123, 82]);
        assert_eq!(tokens("  a   b  "), [43, 16, 44]);
    }

    #[test]
    fn long_texts_split_after_sentences() {
        // a b. c d, e f
        let tokens = [43, 16, 44, 4, 16, 45, 16, 46, 3, 16, 47, 16, 48];
        assert_eq!(chunks(&tokens, 20), [&tokens[..]]);
        assert_eq!(chunks(&tokens, 8), [&tokens[..4], &tokens[5..]]);
        assert_eq!(chunks(&tokens[5..], 5), [&tokens[5..9], &tokens[10..]]);
        assert_eq!(chunks(&[43; 7], 3).len(), 3, "no break anywhere: hard cuts");
    }

    #[test]
    fn wav_header() {
        let file = wav(&[0.0, 1.0, -1.0]);
        assert_eq!(&file[0..4], b"RIFF");
        assert_eq!(file.len(), 44 + 6);
        assert_eq!(&file[44..], [0, 0, 0xFF, 0x7F, 0x01, 0x80]);
    }
}
