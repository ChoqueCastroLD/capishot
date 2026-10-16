//! The tokenizer against Hugging Face's `CLIPTokenizer` (slow and fast agree on every prompt here;
//! `clip_tokens.json` from `scripts/tok_ref.py`). Needs CLIP's `merges.txt` (not committed): set
//! `SD_CLIP_MERGES=<path>`; skipped otherwise.

use photocraft_ai_sd::tokenizer::{CONTEXT, Tokenizer};

#[test]
fn matches_hugging_face() {
    let Ok(path) = std::env::var("SD_CLIP_MERGES") else {
        eprintln!("SD_CLIP_MERGES not set: skipped");
        return;
    };
    let merges = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{e}"));
    let t = Tokenizer::from_merges(&merges).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((t.bos(), t.eos()), (49406, 49407));
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!("clip_tokens.json")).unwrap_or_else(|e| panic!("{e}"));
    let mut bad = Vec::new();
    for c in &cases {
        let prompt = c["prompt"].as_str().unwrap_or_default();
        let want: Vec<u32> = c["ids"].as_array().map(|a| a.iter().filter_map(|v| v.as_u64()).map(|v| v as u32).collect()).unwrap_or_default();
        let (got, _) = t.encode_padded(prompt);
        assert_eq!(want.len(), CONTEXT);
        if got[..] != want[..] {
            bad.push(format!("{prompt:?}\n  got  {:?}\n  want {:?}", &got[..12], &want[..12]));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
