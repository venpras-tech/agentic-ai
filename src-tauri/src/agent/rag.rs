//! Retrieval-augmented context: file attachments chunked + embedded for
//! semantic `search_attached_files` lookups.
//!
//! Embeddings are dependency-free *hashed n-gram* vectors: word unigrams +
//! bigrams and character trigrams are hashed into a fixed-size bucket space,
//! weighted with sublinear TF and L2-normalised. Cosine similarity then ranks
//! chunks. This is not as sharp as a neural embedder, but it is deterministic,
//! offline, allocation-cheap and dramatically better than substring search
//! for "where is X discussed" questions.

use std::path::Path;

use serde::Serialize;

/// Embedding dimensionality (power of two keeps the hash mask cheap).
const EMBED_DIM: usize = 768;
/// Target chunk size in characters.
const CHUNK_CHARS: usize = 1200;
/// Overlap between consecutive chunks.
const CHUNK_OVERLAP: usize = 150;
/// Hard cap on attachment size (8 MiB of text).
const MAX_ATTACHMENT_BYTES: u64 = 8 * 1024 * 1024;
/// Max chunks per attached file.
const MAX_CHUNKS_PER_FILE: usize = 400;

/// One embedded chunk of an attached file.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Character offset of the chunk start in the original text.
    pub offset: usize,
    pub text: String,
    pub embedding: Vec<f32>,
}

/// An attached (indexed) file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachedFile {
    pub path: String,
    pub bytes: u64,
    pub chunk_count: usize,
    #[serde(skip)]
    pub chunks: Vec<Chunk>,
}

/// Session-scoped attachment index.
#[derive(Default)]
pub struct AttachmentIndex {
    files: Vec<AttachedFile>,
}

impl AttachmentIndex {
    pub fn list(&self) -> &[AttachedFile] {
        &self.files
    }

    /// Index `path` (re-indexing replaces any previous version). Returns the
    /// stored summary so callers can render/echo it.
    pub fn attach(&mut self, path: &str, text: &str) -> Result<AttachedFile, String> {
        let meta = std::fs::metadata(Path::new(path)).ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(text.len() as u64);
        if size > MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "Attachment too large ({size} bytes; cap is {MAX_ATTACHMENT_BYTES})"
            ));
        }
        let mut chunks = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        if !chars.is_empty() {
            let mut start = 0usize;
            while start < chars.len() && chunks.len() < MAX_CHUNKS_PER_FILE {
                let end = (start + CHUNK_CHARS).min(chars.len());
                let slice: String = chars[start..end].iter().collect();
                chunks.push(Chunk {
                    offset: start,
                    text: slice.clone(),
                    embedding: embed(&slice),
                });
                if end == chars.len() {
                    break;
                }
                start += CHUNK_CHARS.saturating_sub(CHUNK_OVERLAP).max(1);
            }
        }
        let file = AttachedFile {
            path: path.to_string(),
            bytes: size,
            chunk_count: chunks.len(),
            chunks,
        };
        self.detach(path); // replace previous index for the same path
        self.files.push(file.clone());
        Ok(file)
    }

    pub fn detach(&mut self, path: &str) -> bool {
        let before = self.files.len();
        self.files.retain(|f| f.path != path);
        self.files.len() != before
    }

    /// Reset the index (test helper for isolation between cases).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn clear(&mut self) {
        self.files.clear();
    }

    /// Top-`k` chunks across all attachments ranked by cosine similarity to
    /// the query. Returns `(file_path, char_offset, score, text)`.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<(String, usize, f32, String)> {
        if self.files.is_empty() || query.trim().is_empty() {
            return Vec::new();
        }
        let q = embed(query);
        let mut hits: Vec<(String, usize, f32, String)> = Vec::new();
        for file in &self.files {
            for chunk in &file.chunks {
                let score = cosine(&q, &chunk.embedding);
                hits.push((file.path.clone(), chunk.offset, score, chunk.text.clone()));
            }
        }
        hits.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(top_k.clamp(1, 20));
        hits
    }
}

/// Hashed n-gram embedding: lowercase word unigrams/bigrams + char trigrams,
/// sublinear TF weighting, L2 normalisation.
pub fn embed(text: &str) -> Vec<f32> {
    let lower = text.to_lowercase();
    let mut buckets = vec![0f32; EMBED_DIM];

    // Word unigrams + bigrams.
    let words: Vec<&str> = lower.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        bump(&mut buckets, &format!("w:{w}"));
        if i + 1 < words.len() {
            bump(&mut buckets, &format!("b:{} {}", w, words[i + 1]));
        }
    }

    // Character trigrams over the squeezed string (skips huge gaps).
    let squeezed: String = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = squeezed.chars().collect();
    if chars.len() >= 3 {
        for i in 0..=(chars.len() - 3) {
            let tri: String = chars[i..i + 3].iter().collect();
            bump(&mut buckets, &format!("t:{tri}"));
        }
    }

    // Sublinear TF + L2 norm.
    let mut norm = 0f32;
    for v in buckets.iter_mut() {
        if *v > 0.0 {
            *v = 1.0 + v.ln();
            norm += *v * *v;
        }
    }
    if norm > 0.0 {
        let inv = 1.0 / norm.sqrt();
        for v in buckets.iter_mut() {
            if *v > 0.0 {
                *v *= inv;
            }
        }
    }
    buckets
}

fn bump(buckets: &mut [f32], feature: &str) {
    let h = fnv1a(feature.as_bytes());
    buckets[(h as usize) % buckets.len()] += 1.0;
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn chunking_covers_text_with_overlap() {
        let text = "x".repeat(CHUNK_CHARS * 2 + 500);
        let mut idx = AttachmentIndex::default();
        let file = idx.attach("t.txt", &text).unwrap();
        assert!(file.chunk_count >= 3);

        // Offsets advance and never regress; overlap means step < CHUNK_CHARS.
        let offsets: Vec<usize> = file.chunks.iter().map(|c| c.offset).collect();
        for w in offsets.windows(2) {
            assert!(w[1] > w[0]);
            assert!(w[1] - w[0] <= CHUNK_CHARS);
        }
    }

    #[test]
    fn embedding_is_deterministic_and_normalized() {
        let a = embed("The quick brown fox jumps");
        let b = embed("The quick brown fox jumps");
        assert_eq!(a, b);
        let norm: f32 = a.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
        assert_eq!(embed("").iter().sum::<f32>(), 0.0);
    }

    #[test]
    fn search_ranks_relevant_chunk_first_and_respects_detach() {
        let mut idx = AttachmentIndex::default();
        idx.attach(
            "auth.md",
            "The login flow uses JWT tokens. Passwords are hashed with argon2id \
             before storage. Sessions expire after 24 hours.",
        )
        .unwrap();
        idx.attach(
            "payments.md",
            "Stripe handles card payments. Refunds are issued via the dashboard \
             and webhooks keep invoice state current.",
        )
        .unwrap();

        let hits = idx.search("how are passwords hashed", 2);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0, "auth.md");

        let payments = idx.search("stripe refunds webhook", 1);
        assert_eq!(payments[0].0, "payments.md");

        assert!(idx.detach("auth.md"));
        assert!(!idx.detach("auth.md")); // already gone
        let after = idx.search("password hashing", 5);
        assert!(after.iter().all(|h| h.0 != "auth.md"));

        idx.clear();
        assert!(idx.search("anything", 3).is_empty());
    }

    #[test]
    fn attach_replaces_previous_version() {
        let mut idx = AttachmentIndex::default();
        idx.attach("f.txt", "short").unwrap();
        idx.attach("f.txt", "much longer replacement content goes here")
            .unwrap();
        assert_eq!(idx.list().len(), 1);
        assert_eq!(idx.list()[0].chunk_count, 1);
    }

    #[test]
    fn feature_hashing_is_stable_across_calls() {
        let mut seen = HashMap::new();
        for _ in 0..3 {
            seen.insert(fnv1a(b"w:hello"), ());
        }
        assert_eq!(seen.len(), 1);
    }
}
