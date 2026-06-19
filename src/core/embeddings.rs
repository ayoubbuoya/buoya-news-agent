//! Local text embeddings via fastembed (BGE-small-en-v1.5, 384-dim).
//!
//! The model is loaded once at startup and shared through `AppState`. Inference
//! is synchronous, CPU-bound work, so async callers must run [`Embedder::embed`]
//! inside `tokio::task::spawn_blocking` to avoid stalling the runtime.

use std::sync::Mutex;

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

/// Embedding width produced by BGE-small-en-v1.5. The `vec_articles` table
/// declares `float[EMBED_DIM]`, so changing the model means updating this value
/// and re-embedding every stored article.
pub const EMBED_DIM: usize = 384;

pub struct Embedder {
    // fastembed's `embed` takes `&mut self`; the model is shared via Arc, so the
    // Mutex provides the interior mutability. Inference is serialized CPU work
    // anyway, so contention is not a concern.
    model: Mutex<TextEmbedding>,
}

impl Embedder {
    /// Load the embedding model. On first run this downloads the model weights
    /// (~130 MB) and caches them on disk; subsequent runs load from cache. This
    /// blocks, so callers on an async runtime should use `spawn_blocking`.
    pub fn load() -> Result<Self> {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(false),
        )
        .context("failed to load fastembed embedding model")?;
        Ok(Self {
            model: Mutex::new(model),
        })
    }

    /// Embed a batch of texts into `EMBED_DIM`-wide vectors. Synchronous and
    /// CPU-bound.
    pub fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let mut model = self
            .model
            .lock()
            .map_err(|_| anyhow::anyhow!("embedder mutex poisoned"))?;
        model
            .embed(texts, None)
            .context("embedding inference failed")
    }
}

// TextEmbedding wraps an opaque ONNX session that is not Debug; AppState derives
// Debug, so provide an opaque impl.
impl std::fmt::Debug for Embedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Embedder").finish_non_exhaustive()
    }
}

/// Serialize a float vector to the little-endian `f32` byte layout sqlite-vec
/// expects when binding a vector as a BLOB.
pub fn vec_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for f in v {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        1.0 - dot / (na * nb)
    }

    // Downloads the model on first run, so it's excluded from the default test
    // run. Execute with: `cargo test -- --ignored embeds_with_expected_dim`.
    #[test]
    #[ignore]
    fn embeds_with_expected_dim_and_semantic_ordering() {
        let embedder = Embedder::load().unwrap();
        let texts = vec![
            "Bitcoin price rallies as ETF inflows surge".to_string(),
            "The cryptocurrency market climbs on strong ETF demand".to_string(),
            "A recipe for sourdough bread with rye flour".to_string(),
        ];
        let vecs = embedder.embed(texts).unwrap();

        assert_eq!(vecs.len(), 3);
        assert!(vecs.iter().all(|v| v.len() == EMBED_DIM));

        // The two crypto sentences should be closer to each other than either is
        // to the bread sentence.
        let crypto_pair = cosine_distance(&vecs[0], &vecs[1]);
        let cross = cosine_distance(&vecs[0], &vecs[2]);
        assert!(
            crypto_pair < cross,
            "related texts ({crypto_pair}) should be nearer than unrelated ({cross})"
        );
    }
}
