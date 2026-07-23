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
use crate::domain::{
    AgentCandidate, GOOGLE_SCHOLAR_LIBRARY_ACCESS_FLAG, MANUAL_FULLTEXT_ENABLED_FLAG,
    REQUIRES_MANUAL_PDF_FLAG, RawSourceRecord, ResearchPlan,
};
use crate::download::{
    DownloadedPdf, ManualDownloadRequest, USER_SUPPLIED_LICENSE, client as download_client,
    download_work, import_manual_pdf, manual_download_request, pdf_destination,
};
use crate::nvidia::{NvidiaClient, PageModelInput};
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

#[derive(Debug, Serialize)]
pub struct DownloadFailure {
    pub work_id: String,
    pub error: String,
}

#[derive(Debug, Default, Serialize)]
pub struct DownloadReport {
    pub downloaded: usize,
    pub manual_downloads: Vec<ManualDownloadRequest>,
    pub failures: Vec<DownloadFailure>,
}

enum DownloadOutcome {
    Downloaded(DownloadedPdf),
    AwaitingManual(ManualDownloadRequest),
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

trait FormalVersionResolver {
    async fn lookup_crossref(&self, doi: &str) -> Result<Option<DiscoveredWork>>;
}

struct LiveFormalVersionResolver<'a> {
    settings: &'a Settings,
}

impl FormalVersionResolver for LiveFormalVersionResolver<'_> {
    async fn lookup_crossref(&self, doi: &str) -> Result<Option<DiscoveredWork>> {
        lookup_crossref(self.settings, doi).await
    }
}

pub async fn refresh_formal_metadata(state: &State, settings: &Settings) -> Result<usize> {
    refresh_formal_metadata_with(state, &LiveFormalVersionResolver { settings }).await
}

async fn refresh_formal_metadata_with(
    state: &State,
    resolver: &impl FormalVersionResolver,
) -> Result<usize> {
    let works = state.works_with_statuses(&["discovered", "rejected"])?;
    let mut promoted = 0;
    for (work_id, record) in works {
        if !record.work_type.eq_ignore_ascii_case("preprint")
            || record
                .ids
                .get("arxiv")
                .and_then(|value| normalize_arxiv(value))
                .is_none()
        {
            continue;
        }
        let Some(doi) = record.ids.get("doi").and_then(|value| normalize_doi(value)) else {
            continue;
        };
        match resolver.lookup_crossref(&doi).await {
            Ok(Some(enrichment)) => {
                let formal = enrichment.record.is_verified_formal_publication();
                let merged = if formal {
                    Some(record.merge(enrichment.record))
                } else {
                    None
                };
                let destination_id = if let Some(merged) = merged {
                    promoted += 1;
                    state.upsert_work(&merged, "discovered")?
                } else {
                    work_id
                };
                for raw in &enrichment.raw_records {
                    state.store_raw(&destination_id, raw)?;
                }
            }
            Ok(None) => {
                warn!(
                    doi,
                    "Crossref had no formal metadata for DOI-backed arXiv record"
                );
                state.store_raw(&work_id, &resolution_failure("crossref", &doi, "not_found"))?;
            }
            Err(error) => {
                warn!(doi, %error, "Crossref formal metadata refresh failed");
                state.store_raw(
                    &work_id,
                    &resolution_failure("crossref", &doi, "lookup_failed"),
                )?;
            }
        }
    }
    Ok(promoted)
}

pub async fn screen(state: &State, settings: &Settings, plan: &ResearchPlan) -> Result<usize> {
    state.preserve_research_plan(plan)?;
    let nvidia = NvidiaClient::new(settings)?;
    let promoted = refresh_formal_metadata(state, settings).await?;
    if promoted > 0 {
        info!(
            promoted,
            "promoted DOI-backed arXiv records before screening"
        );
    }
    let works = state.works_with_statuses(&["discovered", "rejected"])?;
    let mut assessments = Vec::with_capacity(works.len());
    let mut relevance_scores = Vec::new();
    for (work_id, mut record) in works {
        let requires_manual_pdf = plan.include_paywalled && !record.has_authorized_fulltext();
        record.flags.insert(
            MANUAL_FULLTEXT_ENABLED_FLAG.to_owned(),
            plan.include_paywalled,
        );
        record
            .flags
            .insert(REQUIRES_MANUAL_PDF_FLAG.to_owned(), requires_manual_pdf);
        record.flags.insert(
            GOOGLE_SCHOLAR_LIBRARY_ACCESS_FLAG.to_owned(),
            plan.use_google_scholar_library_access,
        );
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
            relevance_scores.push(score.normalized);
            record.quality =
                add_relevance(record.quality.clone(), score.logit, score.normalized, plan);
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
    if selected == 0 {
        warn!(
            "{}",
            zero_selection_diagnostic(&relevance_scores, plan.min_relevance_score)
        );
    }
    let disqualified = enrich_selected(state, settings).await?;
    Ok(selected.saturating_sub(disqualified))
}

fn zero_selection_diagnostic(scores: &[f64], threshold: f64) -> String {
    if scores.is_empty() {
        return "screening selected zero papers because none passed the hard academic-value gates; inspect stored rejection reasons before changing thresholds".to_owned();
    }
    let minimum = scores.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mean = scores.iter().sum::<f64>() / scores.len() as f64;
    if threshold > 0.0 {
        format!(
            "screening selected zero papers after reranking {} eligible records (sigmoid relevance min={minimum:.6}, mean={mean:.6}, max={maximum:.6}, configured threshold={threshold:.6}); reranker sigmoid scores are not universally calibrated—set min_relevance_score to 0.0 for rank-only triage or calibrate a nonzero threshold with labeled positives and negatives",
            scores.len()
        )
    } else {
        format!(
            "screening selected zero papers despite rank-only relevance screening of {} eligible records (sigmoid relevance min={minimum:.6}, mean={mean:.6}, max={maximum:.6}); inspect target_papers, post-resolution retraction checks, and stored rejection reasons",
            scores.len()
        )
    }
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
                let manual_enabled = merged
                    .flags
                    .get(MANUAL_FULLTEXT_ENABLED_FLAG)
                    .copied()
                    .unwrap_or(false);
                let requires_manual_pdf = manual_enabled && !merged.has_authorized_fulltext();
                merged
                    .flags
                    .insert(REQUIRES_MANUAL_PDF_FLAG.to_owned(), requires_manual_pdf);
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

pub async fn download_selected(state: &State, settings: &Settings) -> Result<DownloadReport> {
    let works = state.works_with_statuses(&[
        "selected",
        "awaiting-manual-download",
        "error:download",
        "error:manual-download",
    ])?;
    let client = download_client(settings)?;
    let workspace = state.workspace.clone();
    let settings = settings.clone();
    let results = stream::iter(works.into_iter().map(|(work_id, record)| {
        let client = client.clone();
        let workspace = workspace.clone();
        let settings = settings.clone();
        async move {
            let manual_enabled = record
                .flags
                .get(MANUAL_FULLTEXT_ENABLED_FLAG)
                .copied()
                .unwrap_or(false);
            let requires_manual_pdf = record
                .flags
                .get(REQUIRES_MANUAL_PDF_FLAG)
                .copied()
                .unwrap_or(false);
            let manual_path = pdf_destination(&workspace, &work_id);
            let result = if manual_enabled && manual_path.is_file() {
                import_manual_pdf(
                    &workspace,
                    &work_id,
                    &record,
                    settings.max_pdf_bytes,
                )
                .map(DownloadOutcome::Downloaded)
                .with_context(|| {
                    format!(
                        "validate user-supplied PDF at {}; replace it with the correct PDF and retry",
                        manual_path.display()
                    )
                })
            } else if requires_manual_pdf {
                Ok(DownloadOutcome::AwaitingManual(manual_download_request(
                    &workspace,
                    &work_id,
                    &record,
                    "no independently authorized open-access PDF was found",
                )))
            } else {
                match download_work(&client, &settings, &workspace, &work_id, &record).await {
                    Ok(downloaded) => Ok(DownloadOutcome::Downloaded(downloaded)),
                    Err(error) if manual_enabled => {
                        Ok(DownloadOutcome::AwaitingManual(manual_download_request(
                            &workspace,
                            &work_id,
                            &record,
                            format!("automatic authorized download failed: {error:#}"),
                        )))
                    }
                    Err(error) => Err(error),
                }
            };
            (work_id, result)
        }
    }))
    .buffer_unordered(settings.download_workers)
    .collect::<Vec<_>>()
    .await;
    let mut report = DownloadReport::default();
    for (work_id, result) in results {
        match result {
            Ok(DownloadOutcome::Downloaded(pdf)) => {
                state.mark_downloaded(&work_id, &pdf.url, &pdf.path, &pdf.sha256, &pdf.license)?;
                if pdf.user_supplied {
                    state.store_raw(
                        &work_id,
                        &RawSourceRecord {
                            source: "manual-pdf".to_owned(),
                            source_id: pdf.sha256.clone(),
                            retrieved_at: now(),
                            raw: json!({
                                "acquisition": "user-supplied",
                                "source_url": pdf.url,
                                "path": pdf.path,
                                "sha256": pdf.sha256,
                                "reuse_license_asserted": false
                            }),
                        },
                    )?;
                }
                report.downloaded += 1;
            }
            Ok(DownloadOutcome::AwaitingManual(request)) => {
                state.mark_awaiting_manual_download(&work_id)?;
                report.manual_downloads.push(request);
            }
            Err(error) => {
                let error = format!("{error:#}");
                let stage = if pdf_destination(&state.workspace, &work_id).exists() {
                    "manual-download"
                } else {
                    "download"
                };
                state.mark_error(&work_id, stage, &error)?;
                report.failures.push(DownloadFailure { work_id, error });
            }
        }
    }
    Ok(report)
}

pub fn render_downloaded(
    state: &mut State,
    settings: &Settings,
    refresh_existing: bool,
) -> Result<usize> {
    let statuses = if refresh_existing {
        &["downloaded", "error:render", "rendered", "indexed"][..]
    } else {
        &["downloaded", "error:render"][..]
    };
    let works = state.works_with_statuses(statuses)?;
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
    let qdrant = QdrantClient::for_corpus(settings, state.corpus_id())?;
    qdrant.ensure_collection().await?;
    let pages = state.pages_for_indexing()?;
    let mut indexed = 0;
    for chunk in pages.chunks(settings.embed_batch_size) {
        let model_inputs = chunk
            .iter()
            .map(|(page, _, _)| PageModelInput {
                image_path: PathBuf::from(&page.image_path),
                text: page.page_text.clone(),
            })
            .collect::<Vec<_>>();
        let vectors = nvidia.embed_pages(&model_inputs).await?;
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
    state: &State,
    settings: &Settings,
    query: &str,
    top_k: usize,
    candidate_limit: usize,
) -> Result<Vec<SearchResult>> {
    if top_k == 0 || candidate_limit == 0 {
        anyhow::bail!("top_k and candidate_limit must be positive");
    }
    let nvidia = NvidiaClient::new(settings)?;
    let qdrant = QdrantClient::for_corpus(settings, state.corpus_id())?;
    qdrant.search(&nvidia, query, top_k, candidate_limit).await
}

pub fn export(state: &State) -> Result<CorpusAudit> {
    let works = state
        .all_works()?
        .into_iter()
        .filter(|(_, _, status)| {
            [
                "selected",
                "awaiting-manual-download",
                "downloaded",
                "rendered",
                "indexed",
            ]
            .contains(&status.as_str())
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
            } else if artifact.pdf_license == USER_SUPPLIED_LICENSE {
                issues.push(
                    "user-supplied PDF has no established reuse license; do not redistribute it"
                        .to_owned(),
                );
            }
        }
        match artifact.status.as_str() {
            "discovered" => issues.push("candidate has not been screened".to_owned()),
            "selected" => issues.push("selected work has not been downloaded".to_owned()),
            "awaiting-manual-download" => {
                issues.push("waiting for the user to supply a lawfully accessed PDF".to_owned())
            }
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

trait CandidateMetadataResolver {
    async fn lookup_arxiv(&self, arxiv_id: &str) -> Result<Option<DiscoveredWork>>;
    async fn lookup_crossref(&self, doi: &str) -> Result<Option<DiscoveredWork>>;
    async fn lookup_openalex(&self, openalex_id: &str) -> Result<Option<DiscoveredWork>>;
    async fn lookup_openalex_by_doi(&self, doi: &str) -> Result<Option<DiscoveredWork>>;
}

struct LiveCandidateMetadataResolver<'a> {
    settings: &'a Settings,
}

impl CandidateMetadataResolver for LiveCandidateMetadataResolver<'_> {
    async fn lookup_arxiv(&self, arxiv_id: &str) -> Result<Option<DiscoveredWork>> {
        lookup_arxiv(self.settings, arxiv_id).await
    }

    async fn lookup_crossref(&self, doi: &str) -> Result<Option<DiscoveredWork>> {
        lookup_crossref(self.settings, doi).await
    }

    async fn lookup_openalex(&self, openalex_id: &str) -> Result<Option<DiscoveredWork>> {
        lookup_openalex(self.settings, openalex_id).await
    }

    async fn lookup_openalex_by_doi(&self, doi: &str) -> Result<Option<DiscoveredWork>> {
        lookup_openalex_by_doi(self.settings, doi).await
    }
}

async fn resolve_agent_candidate(
    settings: &Settings,
    candidate: &AgentCandidate,
) -> Result<DiscoveredWork> {
    resolve_agent_candidate_with(&LiveCandidateMetadataResolver { settings }, candidate).await
}

async fn resolve_agent_candidate_with(
    resolver: &impl CandidateMetadataResolver,
    candidate: &AgentCandidate,
) -> Result<DiscoveredWork> {
    let mut resolutions = Vec::new();
    let mut failures = Vec::new();
    let mut problems = Vec::new();

    if let Some(arxiv_id) = candidate.arxiv_id.as_deref().and_then(normalize_arxiv) {
        collect_resolution(
            "arxiv",
            &arxiv_id,
            2,
            resolver.lookup_arxiv(&arxiv_id).await,
            &mut resolutions,
            &mut failures,
            &mut problems,
        );
    }

    let explicit_openalex = candidate
        .openalex_id
        .as_deref()
        .and_then(crate::util::normalize_openalex);
    if let Some(openalex_id) = &explicit_openalex {
        collect_resolution(
            "openalex",
            openalex_id,
            1,
            resolver.lookup_openalex(openalex_id).await,
            &mut resolutions,
            &mut failures,
            &mut problems,
        );
    }

    let doi = candidate
        .doi
        .as_deref()
        .and_then(normalize_doi)
        .or_else(|| {
            resolutions.iter().find_map(|(_, resolution)| {
                resolution
                    .record
                    .ids
                    .get("doi")
                    .and_then(|value| normalize_doi(value))
            })
        });
    if let Some(doi) = &doi {
        collect_resolution(
            "crossref",
            doi,
            0,
            resolver.lookup_crossref(doi).await,
            &mut resolutions,
            &mut failures,
            &mut problems,
        );

        if explicit_openalex.is_none() {
            match resolver.lookup_openalex_by_doi(doi).await {
                Ok(Some(openalex)) => resolutions.push((1, openalex)),
                Ok(None) => {}
                Err(error) => {
                    warn!(provider = "openalex", identifier = doi, %error, "optional metadata enrichment failed");
                    failures.push(resolution_failure("openalex", doi, "lookup_failed"));
                }
            }
        }
    }

    let Some(resolved) = merge_candidate_resolutions(resolutions, failures) else {
        let detail = if problems.is_empty() {
            "no valid identifier produced an authoritative record".to_owned()
        } else {
            problems.join("; ")
        };
        anyhow::bail!("subagent candidate could not be resolved: {detail}");
    };
    Ok(resolved)
}

fn collect_resolution(
    provider: &str,
    identifier: &str,
    precedence: u8,
    result: Result<Option<DiscoveredWork>>,
    resolutions: &mut Vec<(u8, DiscoveredWork)>,
    failures: &mut Vec<RawSourceRecord>,
    problems: &mut Vec<String>,
) {
    match result {
        Ok(Some(resolution)) => resolutions.push((precedence, resolution)),
        Ok(None) => {
            warn!(
                provider,
                identifier, "scholarly identifier was not resolved"
            );
            failures.push(resolution_failure(provider, identifier, "not_found"));
            problems.push(format!("{provider}:{identifier} was not found"));
        }
        Err(error) => {
            warn!(provider, identifier, %error, "scholarly identifier lookup failed");
            failures.push(resolution_failure(provider, identifier, "lookup_failed"));
            problems.push(format!("{provider}:{identifier} lookup failed"));
        }
    }
}

fn resolution_failure(provider: &str, identifier: &str, status: &str) -> RawSourceRecord {
    RawSourceRecord {
        source: "resolution".to_owned(),
        source_id: format!("{provider}:{identifier}"),
        retrieved_at: now(),
        raw: json!({
            "provider": provider,
            "identifier": identifier,
            "status": status
        }),
    }
}

fn merge_candidate_resolutions(
    mut resolutions: Vec<(u8, DiscoveredWork)>,
    failures: Vec<RawSourceRecord>,
) -> Option<DiscoveredWork> {
    resolutions.sort_by_key(|(precedence, _)| *precedence);
    let mut resolutions = resolutions.into_iter();
    let (_, mut resolved) = resolutions.next()?;
    for (_, enrichment) in resolutions {
        merge_discovered(&mut resolved, enrichment);
    }
    resolved.raw_records.extend(failures);
    Some(resolved)
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
    let download_report = download_selected(state, settings).await?;
    info!(
        downloaded = download_report.downloaded,
        awaiting_manual = download_report.manual_downloads.len(),
        failed = download_report.failures.len(),
        "download stage complete"
    );
    let rendered = render_downloaded(state, settings, false)?;
    info!(rendered, "multimodal PDF page preparation complete");
    let indexed_pages = ingest_pages(state, settings).await?;
    info!(indexed_pages, "Qdrant ingestion complete");
    let audit = export(state)?;
    Ok(json!({
        "discovered": discovered,
        "selected": selected,
        "downloaded": download_report.downloaded,
        "manual_downloads": download_report.manual_downloads,
        "download_failures": download_report.failures,
        "rendered_papers": rendered,
        "indexed_pages": indexed_pages,
        "corpus_audit": audit,
        "state": state.summary()?
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{FullTextCandidate, WorkRecord};
    use std::sync::Mutex;
    use tempfile::tempdir;

    fn discovered(record: WorkRecord, source: &str, source_id: &str) -> DiscoveredWork {
        DiscoveredWork {
            record,
            raw_records: vec![RawSourceRecord {
                source: source.to_owned(),
                source_id: source_id.to_owned(),
                retrieved_at: "2026-01-01T00:00:00Z".to_owned(),
                raw: json!({"source": source}),
            }],
        }
    }

    struct FakeCandidateMetadataResolver {
        arxiv: DiscoveredWork,
        crossref: DiscoveredWork,
        calls: Mutex<Vec<String>>,
    }

    impl CandidateMetadataResolver for FakeCandidateMetadataResolver {
        async fn lookup_arxiv(&self, arxiv_id: &str) -> Result<Option<DiscoveredWork>> {
            self.calls.lock().unwrap().push(format!("arxiv:{arxiv_id}"));
            Ok(Some(self.arxiv.clone()))
        }

        async fn lookup_crossref(&self, doi: &str) -> Result<Option<DiscoveredWork>> {
            self.calls.lock().unwrap().push(format!("crossref:{doi}"));
            Ok(Some(self.crossref.clone()))
        }

        async fn lookup_openalex(&self, openalex_id: &str) -> Result<Option<DiscoveredWork>> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("openalex:{openalex_id}"));
            Ok(None)
        }

        async fn lookup_openalex_by_doi(&self, doi: &str) -> Result<Option<DiscoveredWork>> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("openalex-doi:{doi}"));
            anyhow::bail!("simulated optional enrichment failure")
        }
    }

    struct FakeFormalVersionResolver {
        crossref: DiscoveredWork,
        calls: Mutex<Vec<String>>,
    }

    impl FormalVersionResolver for FakeFormalVersionResolver {
        async fn lookup_crossref(&self, doi: &str) -> Result<Option<DiscoveredWork>> {
            self.calls.lock().unwrap().push(doi.to_owned());
            Ok(Some(self.crossref.clone()))
        }
    }

    #[tokio::test]
    async fn refreshes_rejected_doi_backed_arxiv_records_for_rescreening() {
        let temporary = tempdir().unwrap();
        let state = State::open(&temporary.path().join("corpus")).unwrap();

        let mut preprint = WorkRecord::new("arxiv", "1904.08064v3");
        preprint.ids.insert("arxiv".into(), "1904.08064v3".into());
        preprint
            .ids
            .insert("doi".into(), "10.1016/j.eswa.2020.113680".into());
        preprint.title = "Forecasting with time series imaging".into();
        preprint.abstract_text = "Richer repository abstract for screening.".into();
        preprint.work_type = "preprint".into();
        preprint.container_title = "arXiv".into();
        preprint.quality.rejection_reasons = vec!["preprints excluded by research plan".into()];
        preprint.fulltext_candidates.push(FullTextCandidate {
            url: "https://arxiv.org/pdf/1904.08064v3".into(),
            source: "arxiv".into(),
            authorized: true,
            ..FullTextCandidate::default()
        });
        state.upsert_work(&preprint, "rejected").unwrap();

        let mut formal = WorkRecord::new("crossref", "10.1016/j.eswa.2020.113680");
        formal
            .ids
            .insert("doi".into(), "10.1016/j.eswa.2020.113680".into());
        formal.title = "Forecasting with time series imaging".into();
        formal.work_type = "article-journal".into();
        formal.container_title = "Expert Systems with Applications".into();
        formal.issued.date_parts = vec![vec![2020, 12, 15]];
        formal.url = "https://doi.org/10.1016/j.eswa.2020.113680".into();
        let resolver = FakeFormalVersionResolver {
            crossref: discovered(formal, "crossref", "10.1016/j.eswa.2020.113680"),
            calls: Mutex::new(Vec::new()),
        };

        let promoted = refresh_formal_metadata_with(&state, &resolver)
            .await
            .unwrap();

        assert_eq!(promoted, 1);
        assert_eq!(
            resolver.calls.lock().unwrap().as_slice(),
            ["10.1016/j.eswa.2020.113680"]
        );
        let works = state.all_works().unwrap();
        assert_eq!(works.len(), 1);
        assert_eq!(works[0].0, "doi:10.1016/j.eswa.2020.113680");
        assert_eq!(works[0].2, "discovered");
        assert_eq!(works[0].1.work_type, "article-journal");
        assert_eq!(
            works[0].1.container_title,
            "Expert Systems with Applications"
        );
        assert_eq!(works[0].1.ids["arxiv"], "1904.08064v3");
        assert!(
            works[0]
                .1
                .fulltext_candidates
                .iter()
                .any(|candidate| candidate.source == "arxiv" && candidate.authorized)
        );
    }

    #[tokio::test]
    async fn resolves_and_merges_every_supplied_candidate_identifier() {
        let mut crossref = WorkRecord::new("crossref", "10.1000/formal");
        crossref
            .ids
            .insert("doi".to_owned(), "10.1000/formal".to_owned());
        crossref.title = "A formally published study".to_owned();
        crossref.work_type = "article-journal".to_owned();
        crossref.container_title = "Journal of Evidence".to_owned();
        crossref.publisher = "Scholarly Publisher".to_owned();

        let mut arxiv = WorkRecord::new("arxiv", "2401.00001v2");
        arxiv
            .ids
            .insert("doi".to_owned(), "10.1000/formal".to_owned());
        arxiv
            .ids
            .insert("arxiv".to_owned(), "2401.00001v2".to_owned());
        arxiv.title = "A formally published study".to_owned();
        arxiv.abstract_text = "The repository supplies the screening abstract.".to_owned();
        arxiv.work_type = "preprint".to_owned();
        arxiv.fulltext_candidates.push(FullTextCandidate {
            url: "https://arxiv.org/pdf/2401.00001v2".to_owned(),
            source: "arxiv".to_owned(),
            authorized: true,
            ..FullTextCandidate::default()
        });

        let resolver = FakeCandidateMetadataResolver {
            arxiv: discovered(arxiv, "arxiv", "2401.00001v2"),
            crossref: discovered(crossref, "crossref", "10.1000/formal"),
            calls: Mutex::new(Vec::new()),
        };
        let candidate = AgentCandidate {
            task_id: "task".to_owned(),
            query: "query".to_owned(),
            source: "crossref".to_owned(),
            title: "A formally published study".to_owned(),
            doi: Some("10.1000/formal".to_owned()),
            arxiv_id: Some("2401.00001".to_owned()),
            openalex_id: None,
            landing_url: None,
            evidence_urls: vec!["https://doi.org/10.1000/formal".to_owned()],
            discovered_at: "2026-01-01T00:00:00Z".to_owned(),
        };

        let merged = resolve_agent_candidate_with(&resolver, &candidate)
            .await
            .unwrap();

        assert_eq!(merged.record.ids["doi"], "10.1000/formal");
        assert_eq!(merged.record.ids["arxiv"], "2401.00001v2");
        assert_eq!(merged.record.work_type, "article-journal");
        assert_eq!(merged.record.container_title, "Journal of Evidence");
        assert_eq!(
            merged.record.abstract_text,
            "The repository supplies the screening abstract."
        );
        assert!(
            merged
                .record
                .fulltext_candidates
                .iter()
                .any(|candidate| candidate.source == "arxiv" && candidate.authorized)
        );
        assert!(
            merged
                .record
                .provenance
                .iter()
                .any(|source| source.source == "crossref")
        );
        assert!(
            merged
                .record
                .provenance
                .iter()
                .any(|source| source.source == "arxiv")
        );
        assert!(
            merged
                .raw_records
                .iter()
                .any(|raw| raw.source == "resolution"
                    && raw.source_id == "openalex:10.1000/formal"
                    && raw.raw["status"] == "lookup_failed")
        );
        let calls = resolver.calls.lock().unwrap();
        assert!(calls.contains(&"arxiv:2401.00001".to_owned()));
        assert!(calls.contains(&"crossref:10.1000/formal".to_owned()));
        assert!(calls.contains(&"openalex-doi:10.1000/formal".to_owned()));
    }

    #[test]
    fn zero_selection_diagnostic_recommends_calibration() {
        let message = zero_selection_diagnostic(&[0.002083, 0.030157, 0.005980], 0.35);
        assert!(message.contains("max=0.030157"));
        assert!(message.contains("configured threshold=0.350000"));
        assert!(message.contains("min_relevance_score to 0.0"));
        assert!(message.contains("labeled positives and negatives"));
    }

    #[tokio::test]
    async fn queues_and_resumes_a_user_supplied_pdf() {
        let temporary = tempdir().unwrap();
        let state = State::open(&temporary.path().join("corpus")).unwrap();
        let mut record = WorkRecord::new("crossref", "10.1000/manual");
        record.ids.insert("doi".into(), "10.1000/manual".into());
        record.title = "A subscription article".into();
        record.url = "https://publisher.example/article".into();
        record
            .flags
            .insert(MANUAL_FULLTEXT_ENABLED_FLAG.into(), true);
        record.flags.insert(REQUIRES_MANUAL_PDF_FLAG.into(), true);
        let work_id = state.upsert_work(&record, "selected").unwrap();
        let settings = Settings::load(None).unwrap();

        let waiting = download_selected(&state, &settings).await.unwrap();

        assert_eq!(waiting.downloaded, 0);
        assert_eq!(waiting.manual_downloads.len(), 1);
        assert!(waiting.failures.is_empty());
        assert_eq!(state.all_works().unwrap()[0].2, "awaiting-manual-download");

        let destination = pdf_destination(&state.workspace, &work_id);
        fs::write(&destination, b"%PDF-1.7\nmanual pipeline test").unwrap();
        let resumed = download_selected(&state, &settings).await.unwrap();

        assert_eq!(resumed.downloaded, 1);
        assert!(resumed.manual_downloads.is_empty());
        assert!(resumed.failures.is_empty());
        let details = state.inspect_work(&work_id).unwrap().unwrap();
        assert_eq!(details.summary.status, "downloaded");
        assert_eq!(details.pdf_artifact.unwrap().license, USER_SUPPLIED_LICENSE);
        assert!(
            details
                .provenance_records
                .iter()
                .any(|source| source.source == "manual-pdf")
        );
    }
}
