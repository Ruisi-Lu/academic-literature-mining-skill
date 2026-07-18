use std::env;
use std::path::Path;

use anyhow::{Context, Result, bail};

pub const EMBED_MODEL: &str = "nvidia/llama-nemotron-embed-vl-1b-v2";
pub const RERANK_MODEL: &str = "nvidia/llama-nemotron-rerank-vl-1b-v2";

#[derive(Clone, Debug)]
pub struct Settings {
    pub nvidia_api_key: String,
    pub embed_model: String,
    pub rerank_model: String,
    pub embed_url: String,
    pub rerank_url: String,
    pub openalex_api_key: String,
    pub semantic_scholar_api_key: String,
    pub contact_email: String,
    pub qdrant_url: String,
    pub qdrant_api_key: String,
    pub qdrant_collection: String,
    pub vector_size: usize,
    pub timeout_seconds: u64,
    pub download_workers: usize,
    pub embed_batch_size: usize,
    pub rerank_batch_size: usize,
    pub max_pdf_bytes: u64,
    pub render_dpi: u16,
    pub jpeg_quality: u8,
}

impl Settings {
    pub fn load(env_file: Option<&Path>) -> Result<Self> {
        let _ = dotenvy::dotenv();
        if let Some(path) = env_file {
            dotenvy::from_path_override(path)
                .with_context(|| format!("load environment file {}", path.display()))?;
        }
        let max_pdf_mib = numeric::<u64>("MAX_PDF_MIB", 200)?;
        let max_pdf_bytes = max_pdf_mib
            .checked_mul(1024 * 1024)
            .context("MAX_PDF_MIB is too large")?;
        let settings = Self {
            nvidia_api_key: variable("NVIDIA_API_KEY", ""),
            embed_model: variable("NEMOTRON_EMBED_MODEL_ID", EMBED_MODEL),
            rerank_model: variable("NEMOTRON_RERANK_MODEL_ID", RERANK_MODEL),
            embed_url: variable(
                "NVIDIA_EMBED_URL",
                "https://integrate.api.nvidia.com/v1/embeddings",
            ),
            rerank_url: variable(
                "NVIDIA_RERANK_URL",
                "https://ai.api.nvidia.com/v1/retrieval/nvidia/llama-nemotron-rerank-vl-1b-v2/reranking",
            ),
            openalex_api_key: variable("OPENALEX_API_KEY", ""),
            semantic_scholar_api_key: variable("SEMANTIC_SCHOLAR_API_KEY", ""),
            contact_email: variable("CONTACT_EMAIL", ""),
            qdrant_url: variable("QDRANT_URL", "http://localhost:6333")
                .trim_end_matches('/')
                .to_owned(),
            qdrant_api_key: variable("QDRANT_API_KEY", ""),
            qdrant_collection: variable("QDRANT_COLLECTION", "academic_literature_v1"),
            vector_size: numeric("QDRANT_VECTOR_SIZE", 2048)?,
            timeout_seconds: numeric("HTTP_TIMEOUT_SECONDS", 60)?,
            download_workers: numeric("DOWNLOAD_WORKERS", 4)?,
            embed_batch_size: numeric("NVIDIA_EMBED_BATCH_SIZE", 4)?,
            rerank_batch_size: numeric("NVIDIA_RERANK_BATCH_SIZE", 32)?,
            max_pdf_bytes,
            render_dpi: numeric("PDF_RENDER_DPI", 144)?,
            jpeg_quality: numeric("PDF_JPEG_QUALITY", 85)?,
        };
        settings.validate()?;
        Ok(settings)
    }

    pub fn require_nvidia(&self) -> Result<()> {
        if placeholder(&self.nvidia_api_key) {
            bail!("set a real NVIDIA_API_KEY from build.nvidia.com in .env");
        }
        if self.embed_model != EMBED_MODEL {
            bail!("NEMOTRON_EMBED_MODEL_ID must be {EMBED_MODEL}");
        }
        if self.rerank_model != RERANK_MODEL {
            bail!("NEMOTRON_RERANK_MODEL_ID must be {RERANK_MODEL}");
        }
        Ok(())
    }

    pub fn require_qdrant(&self) -> Result<()> {
        if self.qdrant_url.is_empty() {
            bail!("QDRANT_URL is required");
        }
        if self.qdrant_url.contains("localhost") && placeholder(&self.qdrant_api_key) {
            bail!("set the same non-placeholder QDRANT_API_KEY in .env and Docker Compose");
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.vector_size != 2048 {
            bail!("QDRANT_VECTOR_SIZE must be 2048 for the configured float embedding output");
        }
        if self.timeout_seconds == 0 {
            bail!("HTTP_TIMEOUT_SECONDS must be positive");
        }
        if self.download_workers == 0 {
            bail!("DOWNLOAD_WORKERS must be positive");
        }
        if self.embed_batch_size == 0 {
            bail!("NVIDIA_EMBED_BATCH_SIZE must be positive");
        }
        if self.rerank_batch_size == 0 || self.rerank_batch_size > 1000 {
            bail!("NVIDIA_RERANK_BATCH_SIZE must be between 1 and 1000");
        }
        if self.max_pdf_bytes == 0 {
            bail!("MAX_PDF_MIB must be positive");
        }
        if self.render_dpi == 0 {
            bail!("PDF_RENDER_DPI must be positive");
        }
        if !(1..=100).contains(&self.jpeg_quality) {
            bail!("PDF_JPEG_QUALITY must be between 1 and 100");
        }
        Ok(())
    }
}

fn variable(name: &str, default: &str) -> String {
    env::var(name)
        .unwrap_or_else(|_| default.to_owned())
        .trim()
        .to_owned()
}

fn numeric<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr + Copy + std::fmt::Display,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse()
        .with_context(|| format!("{name} must be a valid positive number"))
}

fn placeholder(value: &str) -> bool {
    value.is_empty() || value.to_lowercase().contains("replace")
}
