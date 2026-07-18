use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use pdfium_auto::{ensure_pdfium_library, is_pdfium_cached};
use serde_json::json;
use tracing_subscriber::EnvFilter;

use crate::config::Settings;
use crate::domain::ResearchPlan;
use crate::pipeline;
use crate::qdrant::QdrantClient;
use crate::state::State;

#[derive(Debug, Parser)]
#[command(name = "litmine", version, about)]
struct Cli {
    #[arg(long, global = true)]
    env_file: Option<PathBuf>,
    #[arg(long, global = true, default_value = "info")]
    log: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize a resumable corpus workspace and SQLite state database.
    Init {
        #[arg(long, default_value = "corpus")]
        workspace: PathBuf,
    },
    /// Validate model configuration and optionally prepare PDFium or check Qdrant.
    Doctor {
        #[arg(long)]
        prepare_pdfium: bool,
        #[arg(long)]
        check_qdrant: bool,
    },
    /// Discover candidates through configured scholarly metadata sources.
    Discover {
        #[arg(long, default_value = "corpus")]
        workspace: PathBuf,
        #[arg(long)]
        plan: PathBuf,
        #[arg(long, default_value_t = 1000)]
        max_candidates: usize,
    },
    /// Apply hard academic-value gates and Nemotron relevance reranking.
    Screen {
        #[arg(long, default_value = "corpus")]
        workspace: PathBuf,
        #[arg(long)]
        plan: PathBuf,
    },
    /// Download independently authorized PDFs for selected works.
    Download {
        #[arg(long, default_value = "corpus")]
        workspace: PathBuf,
    },
    /// Prepare native PDF text plus complete page images without OCR.
    Render {
        #[arg(long, default_value = "corpus")]
        workspace: PathBuf,
        /// Rebuild pages that were already rendered or indexed.
        #[arg(long)]
        refresh_existing: bool,
    },
    /// Embed text-image PDF pages with Nemotron Embed VL and write them to Qdrant.
    Ingest {
        #[arg(long, default_value = "corpus")]
        workspace: PathBuf,
    },
    /// Run discover, screen, download, render, ingest, export, and audit.
    Mine {
        #[arg(long, default_value = "corpus")]
        workspace: PathBuf,
        #[arg(long)]
        plan: PathBuf,
        #[arg(long, default_value_t = 1000)]
        max_candidates: usize,
    },
    /// Retrieve PDF pages from Qdrant and rerank their text-image evidence.
    Query {
        query: String,
        #[arg(long, default_value_t = 10)]
        top_k: usize,
        #[arg(long, default_value_t = 50)]
        candidate_limit: usize,
    },
    /// Export CSL JSON, BibTeX, RIS, canonical JSONL, and corpus audits.
    Export {
        #[arg(long, default_value = "corpus")]
        workspace: PathBuf,
    },
    /// Audit citation completeness, provenance, PDF checksums, and indexing state.
    Audit {
        #[arg(long, default_value = "corpus")]
        workspace: PathBuf,
    },
    /// Import strict NDJSON candidates from untrusted budget search workers.
    ImportAgentResults {
        input: PathBuf,
        #[arg(long, default_value = "corpus")]
        workspace: PathBuf,
    },
    /// Show resumable pipeline status counts.
    Status {
        #[arg(long, default_value = "corpus")]
        workspace: PathBuf,
    },
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&cli.log).unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();
    let settings = Settings::load(cli.env_file.as_deref())?;
    let output = match cli.command {
        Command::Init { workspace } => {
            let state = State::open(&workspace)?;
            json!({"workspace": state.workspace, "status": state.summary()?})
        }
        Command::Doctor {
            prepare_pdfium,
            check_qdrant,
        } => {
            settings.require_nvidia()?;
            if prepare_pdfium {
                let path = ensure_pdfium_library(None)?;
                eprintln!("PDFium ready at {}", path.display());
            }
            if check_qdrant {
                QdrantClient::new(&settings)?.health().await?;
            }
            json!({
                "nvidia_models": {
                    "embed": settings.embed_model,
                    "rerank": settings.rerank_model
                },
                "pdfium_cached": is_pdfium_cached(),
                "qdrant_checked": check_qdrant
            })
        }
        Command::Discover {
            workspace,
            plan,
            max_candidates,
        } => {
            let state = State::open(&workspace)?;
            let plan = ResearchPlan::load(&plan)?;
            let count =
                pipeline::discover_into_state(&state, &settings, &plan, max_candidates).await?;
            json!({"discovered": count, "status": state.summary()?})
        }
        Command::Screen { workspace, plan } => {
            let state = State::open(&workspace)?;
            let plan = ResearchPlan::load(&plan)?;
            let count = pipeline::screen(&state, &settings, &plan).await?;
            json!({"selected": count, "status": state.summary()?})
        }
        Command::Download { workspace } => {
            let state = State::open(&workspace)?;
            let count = pipeline::download_selected(&state, &settings).await?;
            json!({"downloaded": count, "status": state.summary()?})
        }
        Command::Render {
            workspace,
            refresh_existing,
        } => {
            let mut state = State::open(&workspace)?;
            let count = pipeline::render_downloaded(&mut state, &settings, refresh_existing)?;
            json!({"rendered_papers": count, "status": state.summary()?})
        }
        Command::Ingest { workspace } => {
            let mut state = State::open(&workspace)?;
            let count = pipeline::ingest_pages(&mut state, &settings).await?;
            json!({"indexed_pages": count, "status": state.summary()?})
        }
        Command::Mine {
            workspace,
            plan,
            max_candidates,
        } => {
            let mut state = State::open(&workspace)?;
            let plan = ResearchPlan::load(&plan)?;
            pipeline::run_all(&mut state, &settings, &plan, max_candidates).await?
        }
        Command::Query {
            query,
            top_k,
            candidate_limit,
        } => {
            serde_json::to_value(pipeline::query(&settings, &query, top_k, candidate_limit).await?)?
        }
        Command::Export { workspace } => {
            let state = State::open(&workspace)?;
            serde_json::to_value(pipeline::export(&state)?)?
        }
        Command::Audit { workspace } => {
            let state = State::open(&workspace)?;
            serde_json::to_value(pipeline::citation_audit(&state)?)?
        }
        Command::ImportAgentResults { input, workspace } => {
            let state = State::open(&workspace)?;
            let count = pipeline::import_agent_results(&state, &settings, &input).await?;
            json!({"imported": count, "status": state.summary()?})
        }
        Command::Status { workspace } => {
            let state = State::open(&workspace)?;
            json!({"status": state.summary()?})
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
