//! The CLIP byte-level BPE tokenizer (OpenAI CLIP ViT-L/14, as used by Stable Diffusion 1.x), in
//! plain Rust.
//!
//! The vocabulary is derived from the merge list alone, exactly like CLIP's `SimpleTokenizer`:
//! the 256 byte symbols, the same with the end-of-word marker `</w>`, one token per merge in
//! rank order, then `<|startoftext|>` and `<|endoftext|>` (49 408 ids for the 48 894 merges).
//!
//! Text clean-up follows CLIP: whitespace collapsed, lower-cased, then split by the pattern
//! `'s|'t|'re|'ve|'m|'ll|'d|\p{L}+|\p{N}|[^\s\p{L}\p{N}]+`. Letters and digits are
//! `char::is_alphabetic` / `char::is_numeric`, which equal `\p{L}` / `\p{N}` except for a few
//! combining marks (CLIP's `ftfy` NFC normalisation is not applied: prompts typed in a text box
//! are already composed).

use std::collections::HashMap;

/// Token id of `<|startoftext|>`.
pub const BOS: u32 = 49406;
/// Token id of `<|endoftext|>` (also the padding token of SD 1.x).
pub const EOS: u32 = 49407;
/// Context length of the text encoder.
pub const CONTEXT: usize = 77;
/// Number of merges in the CLIP vocabulary.
pub const MERGES: usize = 48894;

/// Errors building the tokenizer.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenizerError {
    /// The merge list is not CLIP's.
    #[error("invalid merge list: {0}")]
    Merges(String),
}

/// A byte-level BPE tokenizer.
#[derive(Debug, Clone)]
pub struct Tokenizer {
    byte_char: [char; 256],
    ranks: HashMap<(String, String), u32>,
    encoder: HashMap<String, u32>,
    bos: u32,
    eos: u32,
}

/// CLIP/GPT-2's reversible byte → printable character table.
fn bytes_to_unicode() -> [(u8, char); 256] {
    let mut bs: Vec<u32> = (u32::from(b'!')..=u32::from(b'~')).chain(0xA1..=0xAC).chain(0xAE..=0xFF).collect();
    let mut cs = bs.clone();
    let mut n = 0;
    for b in 0..256u32 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    let mut out = [(0u8, '\0'); 256];
    for (i, (b, c)) in bs.into_iter().zip(cs).enumerate() {
        if let Some(slot) = out.get_mut(i) {
            *slot = (u8::try_from(b).unwrap_or(0), char::from_u32(c).unwrap_or('\0'));
        }
    }
    out
}

impl Tokenizer {
    /// Builds the tokenizer from CLIP's `merges.txt` (the `#version` header line is optional).
    /// Accepts any merge count up to [`MERGES`] (tests use short lists); the special tokens
    /// follow the merges.
    pub fn from_merges(text: &str) -> Result<Self, TokenizerError> {
        let table = bytes_to_unicode();
        let mut byte_char = ['\0'; 256];
        let mut vocab: Vec<String> = Vec::with_capacity(512 + MERGES + 2);
        for (b, c) in table {
            if let Some(slot) = byte_char.get_mut(usize::from(b)) {
                *slot = c;
            }
            vocab.push(c.to_string());
        }
        for i in 0..256 {
            let v = format!("{}</w>", vocab.get(i).map(String::as_str).unwrap_or_default());
            vocab.push(v);
        }
        let mut ranks = HashMap::new();
        for line in text.lines().filter(|l| !l.starts_with("#version") && !l.trim().is_empty()) {
            let mut parts = line.split(' ');
            let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
                return Err(TokenizerError::Merges(format!("bad line `{}`", line.chars().take(40).collect::<String>())));
            };
            if ranks.len() >= MERGES {
                break;
            }
            let rank = u32::try_from(ranks.len()).map_err(|_| TokenizerError::Merges("too many merges".into()))?;
            ranks.insert((a.to_string(), b.to_string()), rank);
            vocab.push(format!("{a}{b}"));
        }
        if ranks.is_empty() {
            return Err(TokenizerError::Merges("no merges".into()));
        }
        vocab.push("<|startoftext|>".into());
        vocab.push("<|endoftext|>".into());
        let n = u32::try_from(vocab.len()).map_err(|_| TokenizerError::Merges("vocabulary too large".into()))?;
        let mut encoder = HashMap::with_capacity(vocab.len());
        for (i, t) in vocab.into_iter().enumerate() {
            encoder.entry(t).or_insert(u32::try_from(i).unwrap_or(0));
        }
        Ok(Self { byte_char, ranks, encoder, bos: n - 2, eos: n - 1 })
    }

    /// Id of `<|startoftext|>` ([`BOS`] for the full vocabulary).
    pub fn bos(&self) -> u32 {
        self.bos
    }

    /// Id of `<|endoftext|>` ([`EOS`] for the full vocabulary).
    pub fn eos(&self) -> u32 {
        self.eos
    }

    /// BPE of one pre-token (already mapped to byte characters), as vocabulary ids.
    fn bpe(&self, token: &str, out: &mut Vec<u32>) {
        let chars: Vec<char> = token.chars().collect();
        let Some((last, init)) = chars.split_last() else { return };
        let mut word: Vec<String> = init.iter().map(|c| c.to_string()).collect();
        word.push(format!("{last}</w>"));
        while word.len() > 1 {
            // The adjacent pair with the lowest rank.
            let best = word
                .windows(2)
                .filter_map(|w| match w {
                    [a, b] => self.ranks.get(&(a.clone(), b.clone())).map(|r| (*r, a.clone(), b.clone())),
                    _ => None,
                })
                .min_by_key(|(r, _, _)| *r);
            let Some((_, a, b)) = best else { break };
            let mut merged = Vec::with_capacity(word.len());
            let mut i = 0;
            while i < word.len() {
                if word.get(i) == Some(&a) && word.get(i + 1) == Some(&b) {
                    merged.push(format!("{a}{b}"));
                    i += 2;
                } else {
                    if let Some(w) = word.get(i) {
                        merged.push(w.clone());
                    }
                    i += 1;
                }
            }
            word = merged;
        }
        for w in &word {
            // Every symbol is in the vocabulary (bytes and merges are); skip defensively.
            if let Some(id) = self.encoder.get(w) {
                out.push(*id);
            }
        }
    }

    /// Token ids of `text`, without the start/end tokens.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let clean = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        let mut out = Vec::new();
        for piece in pre_tokenize(&clean) {
            let mapped: String = piece.bytes().map(|b| self.byte_char.get(usize::from(b)).copied().unwrap_or('\0')).collect();
            self.bpe(&mapped, &mut out);
        }
        out
    }

    /// The text encoder input: `<|startoftext|>`, at most 75 tokens, `<|endoftext|>`, padded
    /// with `<|endoftext|>` to [`CONTEXT`]. Also returns whether the prompt was truncated.
    pub fn encode_padded(&self, text: &str) -> ([u32; CONTEXT], bool) {
        let ids = self.encode(text);
        let truncated = ids.len() > CONTEXT - 2;
        let mut out = [self.eos; CONTEXT];
        out[0] = self.bos;
        for (slot, id) in out.iter_mut().skip(1).zip(ids.iter().take(CONTEXT - 2)) {
            *slot = *id;
        }
        (out, truncated)
    }
}

/// CLIP's pre-tokenisation of cleaned (lower-case, single-spaced) text.
fn pre_tokenize(text: &str) -> Vec<&str> {
    const CONTRACTIONS: [&str; 7] = ["'s", "'t", "'re", "'ve", "'m", "'ll", "'d"];
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c.is_whitespace() {
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if let Some(special) = ["<|startoftext|>", "<|endoftext|>"].into_iter().find(|s| rest.starts_with(s)) {
            out.push(&rest[..special.len()]);
            rest = &rest[special.len()..];
            continue;
        }
        if let Some(con) = CONTRACTIONS.into_iter().find(|s| rest.starts_with(s)) {
            out.push(&rest[..con.len()]);
            rest = &rest[con.len()..];
            continue;
        }
        let end = if c.is_alphabetic() {
            rest.char_indices().find(|(_, c)| !c.is_alphabetic()).map_or(rest.len(), |(i, _)| i)
        } else if c.is_numeric() {
            c.len_utf8()
        } else {
            // A run of symbols; like the regex alternation, a contraction can't start inside it
            // (the run is matched greedily by `[^\s\p{L}\p{N}]+`).
            rest.char_indices().find(|(_, c)| c.is_whitespace() || c.is_alphabetic() || c.is_numeric()).map_or(rest.len(), |(i, _)| i)
        };
        out.push(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_tokenize_like_clip() {
        assert_eq!(pre_tokenize("it's a cat's 12 hats!!"), vec!["it", "'s", "a", "cat", "'s", "1", "2", "hats", "!!"]);
        assert_eq!(pre_tokenize("señor,niño"), vec!["señor", ",", "niño"]);
    }

    #[test]
    fn tiny_vocabulary() {
        let t = Tokenizer::from_merges("#version: 0.2\nc a\nca t</w>\n").unwrap_or_else(|e| panic!("{e}"));
        // 512 byte symbols, then `ca` (512), `cat</w>` (513), then the specials.
        assert_eq!((t.bos(), t.eos()), (514, 515));
        assert_eq!(t.encode("Cat"), vec![513]);
        // `cats`: c a t s</w> → ca t s</w>; `t`=84 ('t' is byte 116 at index 116-33=83… see table)
        let ids = t.encode("cats");
        assert_eq!(ids.first(), Some(&512));
        assert_eq!(ids.len(), 3);
        let (padded, truncated) = t.encode_padded("cat");
        assert!(!truncated);
        assert_eq!(&padded[..4], &[514, 513, 515, 515]);
    }

    #[test]
    fn bad_merges_are_errors() {
        assert!(Tokenizer::from_merges("").is_err());
        assert!(Tokenizer::from_merges("a b c\n").is_err());
    }

    #[test]
    fn long_prompts_truncate() {
        let t = Tokenizer::from_merges("a b\n").unwrap_or_else(|e| panic!("{e}"));
        let (ids, truncated) = t.encode_padded(&"x ".repeat(100));
        assert!(truncated);
        assert_eq!(ids[CONTEXT - 1], t.eos());
    }
}
