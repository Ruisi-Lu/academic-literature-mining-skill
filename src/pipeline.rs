use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use futures_util::stream::{self, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::citations::{CitationAudit, audit, export_library};
use crate::config::Settings;
use crate::discovery::{
    DiscoveredWork, discover, lookup_arxiv, lookup_crossref, lookup_openalex,
    lookup_openalex_by_doi,
};
use crate::domain::{AgentCandidate, RawSourceRecord, ResearchPlan};
use crate::download::{client as download_client, download_work};
use crate::nvidia::NvidiaClient;
use crate::qdrant::{QdrantClient, SearchResult};
use crate::quality::{add_relevance, assess};
use crate::render::render_pdf;
use crate::state::State;
use crate::util::{atomic_json, normalize_arxiv, normalize_doi, now, sha256_file};

#[derive(Debug, Serialize)]
pub struct CorpusAudit {
    pub clean: bool,
    pub citation: CitationAudit,
    pub artifact_issues: Vec<ArtifactIssue>,
}

#[derive(Debug, Serialize)]
pub struct ArtifactIssue {
    pub work_id: String,
    pub status: String,
    pub issues: Vec<String>,
}

pub async fn discover_into_state(
    state: &State,
    settings: &Settings,
    plan: &ResearchPlan,
    max_candidates: usize,
) -> Result<usize> {
    if max_candidates == 0 {
        anyhow::bail!("max_candidates must be positive");
    }
    state.preserve_research_plan(plan)?;
    let discovered = discover(plan, settings, max_candidates).await?;
    let mut stored = 0;
    for item in discovered {
        let identity = item.record.identity();
        let record = match state.get_work(&identity)? {
            Some(existing) => existing.merge(item.record),
            None => item.record,
        };
        let work_id = state.upsert_work(&record, "discovered")?;
        for raw in &item.raw_records {
            state.store_raw(&work_id, raw)?;
        }
        stored += 1;
    }
    Ok(stored)
}

pub async fn screen(state: &State, settings: &Settings, plan: &ResearchPlan) -> Result<usize> {
    state.preserve_research_plan(plan)?;
    let nvidia = NvidiaClient::new(settings)?;
    let works = state.works_with_statuses(&["discovered", "rejected"])?;
    let mut assessments = Vec::with_capacity(works.len());
    for (work_id, mut record) in works {
        record.quality = assess(&record, plan);
        assessments.push((work_id, record));
    }
    for chunk in assessments.chunks_mut(settings.rerank_batch_size) {
        let eligible = chunk
            .iter()
            .enumerate()
            .filter(|(_, (_, record))| record.quality.accepted)
            .map(|(index, (_, record))| (index, record.screening_passage()))
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            continue;
        }
        let passages = eligible
            .iter()
            .map(|(_, passage)| passage.clone())
            .collect::<Vec<_>>();
        let scores = nvidia
            .rerank_text(&plan.screening_query(), &passages)
            .await?;
        for ((index, _), score) in eligible.into_iter().zip(scores) {
            let record = &mut chunk[index].1;
            record.quality = add_relevance(record.quality.clone(), score, plan);
        }
    }
    assessments.sort_by(|left, right| {
        right
            .1
            .quality
            .priority_score
            .total_cmp(&left.1.quality.priority_score)
    });
    let mut selected = 0;
    for (_, mut record) in assessments {
        let status = if record.quality.accepted && selected < plan.target_papers {
            selected += 1;
            "selected"
        } else {
            if record.quality.accepted {
                record
                    .quality
                    .rejection_reasons
                    .push("outside configured target_papers cap".to_owned());
                record.quality.accepted = false;
            }
            "rejected"
        };
        state.upsert_work(&record, status)?;
    }
    let disqualified = enrich_selected(state, settings).await?;
    Ok(selected.saturating_sub(disqualified))
}

async fn enrich_selected(state: &State, settings: &Settings) -> Result<usize> {
    let selected = state.works_with_statuses(&["selected"])?;
    let mut disqualified = 0;
    for (_, record) in selected {
        let Some(doi) = record.ids.get("doi").and_then(|value| normalize_doi(value)) else {
            continue;
        };
        match lookup_crossref(settings, &doi).await {
            Ok(Some(enrichment)) => {
                let mut merged = record.merge(enrichment.record);
                let status = if merged.flags.get("is_retracted").copied().unwrap_or(false) {
                    merged.quality.accepted = false;
                    merged.quality.rejection_reasons.push(
                        "retracted or withdrawn record found during citation enrichment".into(),
                    );
                    disqualified += 1;
                    "rejected"
                } else {
                    "selected"
                };
                let work_id = state.upsert_work(&merged, status)?;
                for raw in &enrichment.raw_records {
                    state.store_raw(&work_id, raw)?;
                }
            }
            Ok(None) => warn!(doi, "Crossref had no record for selected DOI"),
            Err(error) => warn!(doi, %error, "Crossref enrichment failed"),
        }
    }
    Ok(disqualified)
}

pub async fn download_selected(state: &State, settings: &Settings) -> Result<usize> {
    let works = state.works_with_statuses(&["selected", "error:download"])?;
    let client = download_client(settings)?;
    let workspace = state.workspace.clone();
    let settings = settings.clone();
    let results = stream::iter(works.into_iter().map(|(work_id, record)| {
        let client = client.clone();
        let workspace = workspace.clone();
        let settings = settings.clone();
        async move {
            let result = download_work(&client, &settings, &workspace, &work_id, &record).await;
            (work_id, result)
        }
    }))
    .buffer_unordered(settings.download_workers)
    .collect::<Vec<_>>()
    .await;
    let mut downloaded = 0;
    for (work_id, result) in results {
        match result {
            Ok(pdf) => {
                state.mark_downloaded(&work_id, &pdf.url, &pdf.path, &pdf.sha256, &pdf.license)?;
                downloaded += 1;
            }
            Err(error) => state.mark_error(&work_id, "download", &format!("{error:#}"))?,
        }
    }
    Ok(downloaded)
}

pub fn render_downloaded(state: &mut State, settings: &Settings) -> Result<usize> {
    let works = state.works_with_statuses(&["downloaded", "error:render"])?;
    let mut rendered = 0;
    for (work_id, _) in works {
        let Some(pdf_path) = state.pdf_for_work(&work_id)? else {
            state.mark_error(&work_id, "render", "downloaded record has no PDF path")?;
            continue;
        };
        match render_pdf(settings, &state.workspace, &work_id, &pdf_path) {
            Ok(pages) if !pages.is_empty() => {
                state.replace_pages(&work_id, &pages)?;
                rendered += 1;
            }
            Ok(_) => state.mark_error(&work_id, "render", "PDF contained no renderable pages")?,
            Err(error) => state.mark_error(&work_id, "render", &format!("{error:#}"))?,
        }
    }
    Ok(rendered)
}

pub async fn ingest_pages(state: &mut State, settings: &Settings) -> Result<usize> {
    let nvidia = NvidiaClient::new(settings)?;
    let qdrant = QdrantClient::new(settings)?;
    qdrant.ensure_collection().await?;
    let pages = state.pages_for_indexing()?;
    let mut indexed = 0;
    for chunk in pages.chunks(settings.embed_batch_size) {
        let paths = chunk
            .iter()
            .map(|(page, _, _)| PathBuf::from(&page.image_path))
            .collect::<Vec<_>>();
        let vectors = nvidia.embed_images(&paths).await?;
        if vectors
            .iter()
            .any(|vector| vector.len() != settings.vector_size)
        {
            anyhow::bail!(
                "NVIDIA embedding dimension does not match QDRANT_VECTOR_SIZE={}",
                settings.vector_size
            );
        }
        qdrant.upsert_pages(chunk, &vectors).await?;
        let page_records = chunk
            .iter()
            .map(|(page, _, _)| page.clone())
            .collect::<Vec<_>>();
        state.mark_pages_indexed(&page_records)?;
        indexed += chunk.len();
    }
    Ok(indexed)
}

pub async fn query(
    settings: &Settings,
    query: &str,
    top_k: usize,
    candidate_limit: usize,
) -> Result<Vec<SearchResult>> {
    if top_k == 0 || candidate_limit == 0 {
        anyhow::bail!("top_k and candidate_limit must be positive");
    }
    let nvidia = NvidiaClient::new(settings)?;
    let qdrant = QdrantClient::new(settings)?;
    qdrant.search(&nvidia, query, top_k, candidate_limit).await
}

pub fn export(state: &State) -> Result<CorpusAudit> {
    let works = state
        .all_works()?
        .into_iter()
        .filter(|(_, _, status)| {
            ["selected", "downloaded", "rendered", "indexed"].contains(&status.as_str())
        })
        .map(|(id, record, _)| (id, record))
        .collect::<Vec<_>>();
    let citation = export_library(&state.workspace, &works)?;
    let audit = build_corpus_audit(state, citation)?;
    atomic_json(
        &state.workspace.join("exports").join("corpus-audit.json"),
        &audit,
    )?;
    Ok(audit)
}

pub fn citation_audit(state: &State) -> Result<CorpusAudit> {
    let works = state
        .all_works()?
        .into_iter()
        .filter(|(_, _, status)| status != "rejected")
        .map(|(id, record, _)| (id, record))
        .collect::<Vec<_>>();
    let audit = build_corpus_audit(state, audit(&works))?;
    atomic_json(
        &state.workspace.join("exports").join("corpus-audit.json"),
        &audit,
    )?;
    Ok(audit)
}

fn build_corpus_audit(state: &State, citation: CitationAudit) -> Result<CorpusAudit> {
    let works = state
        .all_works()?
        .into_iter()
        .map(|(work_id, record, _)| (work_id, record))
        .collect::<HashMap<_, _>>();
    let mut artifact_issues = Vec::new();
    for artifact in state.artifact_statuses()? {
        if artifact.status == "rejected" {
            continue;
        }
        let mut issues = Vec::new();
        if works
            .get(&artifact.work_id)
            .is_none_or(|record| record.provenance.is_empty())
        {
            issues.push("canonical record has no metadata provenance".to_owned());
        }
        if artifact.status.starts_with("error:") {
            issues.push(format!(
                "pipeline failed: {}",
                if artifact.last_error.is_empty() {
                    "no error detail was retained"
                } else {
                    &artifact.last_error
                }
            ));
        }
        let has_pdf = !artifact.pdf_path.is_empty()
            || matches!(
                artifact.status.as_str(),
                "downloaded" | "rendered" | "indexed"
            );
        if has_pdf {
            if artifact.pdf_url.is_empty()
                || artifact.pdf_path.is_empty()
                || artifact.pdf_sha256.is_empty()
            {
                issues.push("preserved PDF artifact metadata is incomplete".to_owned());
            } else {
                let path = Path::new(&artifact.pdf_path);
                if !path.is_file() {
                    issues.push("preserved PDF file is missing".to_owned());
                } else {
                    match sha256_file(path) {
                        Ok(actual) if actual != artifact.pdf_sha256 => {
                            issues.push("preserved PDF SHA-256 does not match".to_owned());
                        }
                        Err(error) => issues.push(format!("could not hash preserved PDF: {error}")),
                        _ => {}
                    }
                }
            }
            if artifact.pdf_license.is_empty() {
                issues.push("PDF has no explicit reuse-license metadata".to_owned());
            }
        }
        match artifact.status.as_str() {
            "discovered" => issues.push("candidate has not been screened".to_owned()),
            "selected" => issues.push("selected work has not been downloaded".to_owned()),
            "downloaded" => issues.push("downloaded PDF has not been rendered".to_owned()),
            "rendered" => issues.push("rendered pages have not all been indexed".to_owned()),
            "indexed" => {
                if artifact.declared_page_count == 0
                    || artifact.declared_page_count != artifact.stored_page_count
                {
                    issues.push("stored page count does not match the work record".to_owned());
                }
                if artifact.indexed_page_count != artifact.stored_page_count {
                    issues.push("one or more rendered pages are not marked indexed".to_owned());
                }
            }
            _ => {}
        }
        if !issues.is_empty() {
            artifact_issues.push(ArtifactIssue {
                work_id: artifact.work_id,
                status: artifact.status,
                issues,
            });
        }
    }
    let clean = citation.incomplete_records.is_empty() && artifact_issues.is_empty();
    Ok(CorpusAudit {
        clean,
        citation,
        artifact_issues,
    })
}

pub async fn import_agent_results(
    state: &State,
    settings: &Settings,
    path: &Path,
) -> Result<usize> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut imported = 0;
    let mut seen = HashMap::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let candidate: AgentCandidate = serde_json::from_str(&line)
            .with_context(|| format!("invalid agent result on line {}", index + 1))?;
        candidate.validate()?;
        let resolved = resolve_agent_candidate(settings, &candidate).await?;
        let record = resolved.record;
        let identity = record.identity();
        if seen.insert(identity.clone(), ()).is_some() {
            continue;
        }
        let record = match state.get_work(&identity)? {
            Some(existing) => existing.merge(record),
            None => record,
        };
        let work_id = state.upsert_work(&record, "discovered")?;
        for raw in &resolved.raw_records {
            state.store_raw(&work_id, raw)?;
        }
        state.store_raw(
            &work_id,
            &RawSourceRecord {
                source: "subagent".to_owned(),
                source_id: format!("{}:{}", candidate.task_id, index + 1),
                retrieved_at: now(),
                raw: serde_json::to_value(&candidate)?,
            },
        )?;
        imported += 1;
    }
    Ok(imported)
}

async fn resolve_agent_candidate(
    settings: &Settings,
    candidate: &AgentCandidate,
) -> Result<DiscoveredWork> {
    if let Some(doi) = candidate.doi.as_deref().and_then(normalize_doi) {
        let mut resolved = lookup_crossref(settings, &doi).await?.with_context(|| {
            format!("subagent DOI could not be resolved through Crossref: {doi}")
        })?;
        if let Some(openalex) = lookup_openalex_by_doi(settings, &doi).await? {
            merge_discovered(&mut resolved, openalex);
        }
        return Ok(resolved);
    }
    if let Some(arxiv_id) = candidate.arxiv_id.as_deref().and_then(normalize_arxiv) {
        let mut resolved = lookup_arxiv(settings, &arxiv_id)
            .await?
            .with_context(|| format!("subagent arXiv ID could not be resolved: {arxiv_id}"))?;
        enrich_resolved_doi(settings, &mut resolved).await?;
        return Ok(resolved);
    }
    let openalex_id = candidate
        .openalex_id
        .as_deref()
        .context("validated candidate lost its OpenAlex ID")?;
    let mut resolved = lookup_openalex(settings, openalex_id)
        .await?
        .with_context(|| format!("subagent OpenAlex ID could not be resolved: {openalex_id}"))?;
    enrich_resolved_doi(settings, &mut resolved).await?;
    Ok(resolved)
}

async fn enrich_resolved_doi(settings: &Settings, resolved: &mut DiscoveredWork) -> Result<()> {
    let Some(doi) = resolved
        .record
        .ids
        .get("doi")
        .and_then(|value| normalize_doi(value))
    else {
        return Ok(());
    };
    if let Some(crossref) = lookup_crossref(settings, &doi).await? {
        merge_discovered(resolved, crossref);
    }
    Ok(())
}

fn merge_discovered(target: &mut DiscoveredWork, enrichment: DiscoveredWork) {
    target.record = target.record.clone().merge(enrichment.record);
    target.raw_records.extend(enrichment.raw_records);
}

pub async fn run_all(
    state: &mut State,
    settings: &Settings,
    plan: &ResearchPlan,
    max_candidates: usize,
) -> Result<Value> {
    let discovered = discover_into_state(state, settings, plan, max_candidates).await?;
    info!(discovered, "discovery complete");
    let selected = screen(state, settings, plan).await?;
    info!(selected, "screening complete");
    let downloaded = download_selected(state, settings).await?;
    info!(downloaded, "downloads complete");
    let rendered = render_downloaded(state, settings)?;
    info!(rendered, "visual page rendering complete");
    let indexed_pages = ingest_pages(state, settings).await?;
    info!(indexed_pages, "Qdrant ingestion complete");
    let audit = export(state)?;
    Ok(json!({
        "discovered": discovered,
        "selected": selected,
        "downloaded": downloaded,
        "rendered_papers": rendered,
        "indexed_pages": indexed_pages,
        "corpus_audit": audit,
        "state": state.summary()?
    }))
}
