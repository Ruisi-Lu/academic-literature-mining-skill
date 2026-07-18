use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate, Utc};
use quick_xml::de::from_str;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::sleep;
use tracing::{info, warn};
use url::Url;

use crate::config::Settings;
use crate::domain::{
    Author, DateParts, FullTextCandidate, Metrics, RawSourceRecord, ResearchPlan, WorkRecord,
};
use crate::util::{
    arxiv_base_id, arxiv_ids_match, normalize_arxiv, normalize_doi, normalize_openalex,
    normalize_space, now, strip_markup, title_fingerprint,
};

#[derive(Clone, Debug)]
pub struct DiscoveredWork {
    pub record: WorkRecord,
    pub raw_records: Vec<RawSourceRecord>,
}

pub async fn discover(
    plan: &ResearchPlan,
    settings: &Settings,
    max_candidates: usize,
) -> Result<Vec<DiscoveredWork>> {
    let client = discovery_client(settings)?;
    let per_search = (max_candidates / plan.queries.len().max(1) / plan.sources.len().max(1))
        .max(25)
        .min(max_candidates);
    let mut found = Vec::new();
    let mut failures = Vec::new();

    for source in &plan.sources {
        for query in &plan.queries {
            let result = match source.as_str() {
                "openalex" => {
                    if !openalex_key_is_set(settings) {
                        warn!("OPENALEX_API_KEY is not set; skipping OpenAlex");
                        failures.push(format!(
                            "openalex:{query}: OPENALEX_API_KEY is not configured"
                        ));
                        Ok(Vec::new())
                    } else {
                        search_openalex(&client, settings, plan, query, per_search).await
                    }
                }
                "crossref" => search_crossref(&client, settings, plan, query, per_search).await,
                "arxiv" => search_arxiv(&client, plan, query, per_search).await,
                unsupported => bail!("unsupported source {unsupported}"),
            };
            let mut batch = match result {
                Ok(batch) => batch,
                Err(error) => {
                    warn!(source, query, %error, "discovery shard failed");
                    failures.push(format!("{source}:{query}: {error:#}"));
                    continue;
                }
            };
            info!(
                source,
                query,
                count = batch.len(),
                "discovery batch complete"
            );
            found.append(&mut batch);
            if found.len() >= max_candidates.saturating_mul(2) {
                break;
            }
        }
    }
    if found.is_empty() && !failures.is_empty() {
        bail!("all discovery shards failed:\n{}", failures.join("\n"));
    }

    let mut merged = merge_discovered(found);
    if !settings.semantic_scholar_api_key.is_empty()
        && let Err(error) = enrich_semantic_scholar(&client, settings, &mut merged).await
    {
        warn!(%error, "optional Semantic Scholar enrichment failed");
    }
    merged.truncate(max_candidates);
    Ok(merged)
}

pub async fn lookup_crossref(settings: &Settings, doi: &str) -> Result<Option<DiscoveredWork>> {
    let client = discovery_client(settings)?;
    let mut url = Url::parse("https://api.crossref.org/works/")?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Crossref base URL cannot contain path segments"))?
        .push(doi);
    let response = retry_get(&client, url.as_str(), &[]).await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response.error_for_status()?;
    let payload: Value = response.json().await?;
    let item = payload
        .get("message")
        .cloned()
        .context("Crossref response did not contain message")?;
    let record = crossref_record(&item)?;
    let source_id = record
        .ids
        .get("doi")
        .cloned()
        .unwrap_or_else(|| doi.to_owned());
    Ok(Some(DiscoveredWork {
        record,
        raw_records: vec![RawSourceRecord {
            source: "crossref".to_owned(),
            source_id,
            retrieved_at: now(),
            raw: item,
        }],
    }))
}

pub async fn lookup_openalex(
    settings: &Settings,
    openalex_id: &str,
) -> Result<Option<DiscoveredWork>> {
    let id = normalize_openalex(openalex_id).context("invalid OpenAlex work ID")?;
    require_openalex_key(settings)?;
    let client = discovery_client(settings)?;
    let url = format!("https://api.openalex.org/works/{id}");
    let params = [("api_key", settings.openalex_api_key.clone())];
    let response = retry_get(&client, &url, &params).await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let item: Value = response.error_for_status()?.json().await?;
    Ok(Some(openalex_discovered(item)?))
}

pub async fn lookup_openalex_by_doi(
    settings: &Settings,
    doi: &str,
) -> Result<Option<DiscoveredWork>> {
    if !openalex_key_is_set(settings) {
        return Ok(None);
    }
    let client = discovery_client(settings)?;
    let params = [
        ("filter", format!("doi:{doi}")),
        ("per-page", "1".to_owned()),
        ("api_key", settings.openalex_api_key.clone()),
    ];
    let response = retry_get(&client, "https://api.openalex.org/works", &params)
        .await?
        .error_for_status()?;
    let payload: Value = response.json().await?;
    let Some(item) = payload
        .get("results")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .cloned()
    else {
        return Ok(None);
    };
    Ok(Some(openalex_discovered(item)?))
}

pub async fn lookup_arxiv(settings: &Settings, arxiv_id: &str) -> Result<Option<DiscoveredWork>> {
    let id = normalize_arxiv(arxiv_id).context("invalid arXiv ID")?;
    let client = discovery_client(settings)?;
    let params = [("id_list", id.clone()), ("max_results", "1".to_owned())];
    let response = retry_get(&client, "https://export.arxiv.org/api/query", &params)
        .await?
        .error_for_status()?;
    let text = response.text().await?;
    let feed: ArxivFeed = from_str(&text).context("parse arXiv Atom feed")?;
    let Some(entry) = feed
        .entries
        .into_iter()
        .find(|entry| arxiv_ids_match(&id, &entry.id))
    else {
        return Ok(None);
    };
    let resolved_id =
        normalize_arxiv(&entry.id).context("resolved arXiv entry missing identifier")?;
    let raw = serde_json::to_value(&entry)?;
    let record = arxiv_record(&entry)?;
    Ok(Some(DiscoveredWork {
        record,
        raw_records: vec![RawSourceRecord {
            source: "arxiv".to_owned(),
            source_id: resolved_id,
            retrieved_at: now(),
            raw,
        }],
    }))
}

fn discovery_client(settings: &Settings) -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent(format!(
            "AcademicLiteratureMining/0.1 ({})",
            if settings.contact_email.is_empty() {
                "no-contact"
            } else {
                &settings.contact_email
            }
        ))
        .build()
        .map_err(Into::into)
}

fn openalex_key_is_set(settings: &Settings) -> bool {
    !settings.openalex_api_key.is_empty()
        && !settings.openalex_api_key.to_lowercase().contains("replace")
}

fn require_openalex_key(settings: &Settings) -> Result<()> {
    if !openalex_key_is_set(settings) {
        bail!("OPENALEX_API_KEY is required to resolve an OpenAlex-only candidate");
    }
    Ok(())
}

fn openalex_discovered(item: Value) -> Result<DiscoveredWork> {
    let record = openalex_record(&item)?;
    let source_id = record
        .ids
        .get("openalex")
        .cloned()
        .unwrap_or_else(|| record.identity());
    Ok(DiscoveredWork {
        record,
        raw_records: vec![RawSourceRecord {
            source: "openalex".to_owned(),
            source_id,
            retrieved_at: now(),
            raw: item,
        }],
    })
}

async fn search_openalex(
    client: &Client,
    settings: &Settings,
    plan: &ResearchPlan,
    query: &str,
    limit: usize,
) -> Result<Vec<DiscoveredWork>> {
    let mut output = Vec::new();
    let mut cursor = "*".to_owned();
    while output.len() < limit {
        let mut filters = vec![
            "is_retracted:false".to_owned(),
            "has_abstract:true".to_owned(),
        ];
        if let Some(value) = &plan.date_from {
            filters.push(format!("from_publication_date:{value}"));
        }
        if let Some(value) = &plan.date_to {
            filters.push(format!("to_publication_date:{value}"));
        }
        let per_page = (limit - output.len()).min(100).to_string();
        let params = vec![
            ("search", query.to_owned()),
            ("filter", filters.join(",")),
            ("per-page", per_page),
            ("cursor", cursor.clone()),
            ("api_key", settings.openalex_api_key.clone()),
        ];
        let response = retry_get(client, "https://api.openalex.org/works", &params)
            .await?
            .error_for_status()?;
        let payload: Value = response.json().await?;
        let results = payload
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if results.is_empty() {
            break;
        }
        for item in results {
            let record = openalex_record(&item)?;
            let source_id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            output.push(DiscoveredWork {
                record,
                raw_records: vec![RawSourceRecord {
                    source: "openalex".to_owned(),
                    source_id,
                    retrieved_at: now(),
                    raw: item,
                }],
            });
            if output.len() >= limit {
                break;
            }
        }
        let next = payload
            .pointer("/meta/next_cursor")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if next.is_empty() || next == cursor {
            break;
        }
        cursor = next.to_owned();
    }
    Ok(output)
}

async fn search_crossref(
    client: &Client,
    settings: &Settings,
    plan: &ResearchPlan,
    query: &str,
    limit: usize,
) -> Result<Vec<DiscoveredWork>> {
    let mut output = Vec::new();
    let mut cursor = "*".to_owned();
    while output.len() < limit {
        let mut filters = Vec::new();
        if let Some(value) = &plan.date_from {
            filters.push(format!("from-pub-date:{value}"));
        }
        if let Some(value) = &plan.date_to {
            filters.push(format!("until-pub-date:{value}"));
        }
        let rows = (limit - output.len()).min(100).to_string();
        let mut params = vec![
            ("query.bibliographic", query.to_owned()),
            ("rows", rows),
            ("cursor", cursor.clone()),
        ];
        if !filters.is_empty() {
            params.push(("filter", filters.join(",")));
        }
        if !settings.contact_email.is_empty() {
            params.push(("mailto", settings.contact_email.clone()));
        }
        let response = retry_get(client, "https://api.crossref.org/works", &params)
            .await?
            .error_for_status()?;
        let payload: Value = response.json().await?;
        let items = payload
            .pointer("/message/items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            break;
        }
        for item in items {
            let record = crossref_record(&item)?;
            let source_id = record
                .ids
                .get("doi")
                .cloned()
                .unwrap_or_else(|| record.identity());
            output.push(DiscoveredWork {
                record,
                raw_records: vec![RawSourceRecord {
                    source: "crossref".to_owned(),
                    source_id,
                    retrieved_at: now(),
                    raw: item,
                }],
            });
            if output.len() >= limit {
                break;
            }
        }
        let next = payload
            .pointer("/message/next-cursor")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if next.is_empty() || next == cursor {
            break;
        }
        cursor = next.to_owned();
    }
    Ok(output)
}

async fn search_arxiv(
    client: &Client,
    plan: &ResearchPlan,
    query: &str,
    limit: usize,
) -> Result<Vec<DiscoveredWork>> {
    let mut output = Vec::new();
    let mut start = 0;
    while output.len() < limit {
        let count = (limit - output.len()).min(100);
        let params = vec![
            ("search_query", format!("all:\"{query}\"")),
            ("start", start.to_string()),
            ("max_results", count.to_string()),
            ("sortBy", "relevance".to_owned()),
            ("sortOrder", "descending".to_owned()),
        ];
        let response = retry_get(client, "https://export.arxiv.org/api/query", &params)
            .await?
            .error_for_status()?;
        let text = response.text().await?;
        let feed: ArxivFeed = from_str(&text).context("parse arXiv Atom feed")?;
        if feed.entries.is_empty() {
            break;
        }
        for entry in feed.entries {
            let record = arxiv_record(&entry)?;
            if in_date_range(&record, plan) {
                let source_id = record
                    .ids
                    .get("arxiv")
                    .cloned()
                    .unwrap_or_else(|| record.identity());
                output.push(DiscoveredWork {
                    record,
                    raw_records: vec![RawSourceRecord {
                        source: "arxiv".to_owned(),
                        source_id,
                        retrieved_at: now(),
                        raw: serde_json::to_value(&entry)?,
                    }],
                });
            }
            if output.len() >= limit {
                break;
            }
        }
        start += count;
        sleep(Duration::from_secs(3)).await;
    }
    Ok(output)
}

fn openalex_record(item: &Value) -> Result<WorkRecord> {
    let source_id = string(item, "/id");
    let mut record = WorkRecord::new("openalex", &source_id);
    record.ids.insert("openalex".to_owned(), source_id.clone());
    for (name, pointer) in [
        ("doi", "/ids/doi"),
        ("pmid", "/ids/pmid"),
        ("mag", "/ids/mag"),
    ] {
        let value = string(item, pointer);
        if !value.is_empty() {
            let normalized = if name == "doi" {
                normalize_doi(&value).unwrap_or(value)
            } else {
                value
            };
            record.ids.insert(name.to_owned(), normalized);
        }
    }
    record.title = string(item, "/title");
    record.abstract_text = abstract_from_inverted(item.get("abstract_inverted_index"));
    record.work_type = work_type(&string(item, "/type"));
    record.language = string(item, "/language");
    record.url = [
        string(item, "/doi"),
        string(item, "/primary_location/landing_page_url"),
        source_id.clone(),
    ]
    .into_iter()
    .find(|value| !value.is_empty())
    .unwrap_or_default();
    record.publisher = string(item, "/primary_location/source/host_organization_name");
    record.container_title = string(item, "/primary_location/source/display_name");
    record.volume = string(item, "/biblio/volume");
    record.issue = string(item, "/biblio/issue");
    record.page = page_range(
        &string(item, "/biblio/first_page"),
        &string(item, "/biblio/last_page"),
    );
    record.issued = date_parts(&string(item, "/publication_date"));
    record.authors = item
        .get("authorships")
        .and_then(Value::as_array)
        .map(|values| values.iter().map(openalex_author).collect())
        .unwrap_or_default();
    record.subjects = item
        .get("topics")
        .and_then(Value::as_array)
        .map(|topics| {
            topics
                .iter()
                .filter_map(|topic| topic.get("display_name").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    record.keywords = item
        .get("keywords")
        .and_then(Value::as_array)
        .map(|keywords| {
            keywords
                .iter()
                .filter_map(|keyword| keyword.get("display_name").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    record.issn = strings(item.pointer("/primary_location/source/issn"));
    record.metrics = Metrics {
        cited_by_count: item
            .get("cited_by_count")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        influential_citation_count: 0,
        fwci: item.get("fwci").and_then(Value::as_f64),
        citation_percentile: item
            .pointer("/citation_normalized_percentile/value")
            .and_then(Value::as_f64),
    };
    record.flags.insert(
        "is_retracted".to_owned(),
        item.get("is_retracted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    record.flags.insert(
        "is_paratext".to_owned(),
        item.get("is_paratext")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    let is_oa = item
        .pointer("/open_access/is_oa")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(locations) = item.get("locations").and_then(Value::as_array) {
        for location in locations {
            let pdf_url = string(location, "/pdf_url");
            if !pdf_url.is_empty() {
                let license = string(location, "/license");
                let location_is_oa = location
                    .get("is_oa")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let authorized =
                    location_is_oa || is_open_license(&license) || repository_url(&pdf_url);
                record.fulltext_candidates.push(FullTextCandidate {
                    url: pdf_url,
                    source: "openalex".to_owned(),
                    version: string(location, "/version"),
                    authorized,
                    license,
                    content_type: "application/pdf".to_owned(),
                });
            }
        }
    }
    if !record
        .fulltext_candidates
        .iter()
        .any(|candidate| candidate.authorized)
    {
        let pdf_url = string(item, "/best_oa_location/pdf_url");
        if !pdf_url.is_empty() {
            let license = string(item, "/best_oa_location/license");
            let authorized = is_oa || is_open_license(&license) || repository_url(&pdf_url);
            record.fulltext_candidates.push(FullTextCandidate {
                url: pdf_url,
                source: "openalex".to_owned(),
                version: string(item, "/best_oa_location/version"),
                authorized,
                license,
                content_type: "application/pdf".to_owned(),
            });
        }
    }
    Ok(record)
}

fn crossref_record(item: &Value) -> Result<WorkRecord> {
    let source_id = string(item, "/DOI");
    let mut record = WorkRecord::new("crossref", &source_id);
    if let Some(doi) = normalize_doi(&source_id) {
        record.ids.insert("doi".to_owned(), doi);
    }
    record.title = first_string(item.get("title"));
    record.abstract_text = strip_markup(&string(item, "/abstract"));
    record.work_type = work_type(&string(item, "/type"));
    record.container_title = first_string(item.get("container-title"));
    record.publisher = string(item, "/publisher");
    record.volume = string(item, "/volume");
    record.issue = string(item, "/issue");
    record.page = string(item, "/page");
    record.article_number = string(item, "/article-number");
    record.language = string(item, "/language");
    record.url = string(item, "/URL");
    record.issued = crossref_date(item);
    record.issn = strings(item.get("ISSN"));
    record.isbn = strings(item.get("ISBN"));
    record.subjects = strings(item.get("subject"));
    record.authors = item
        .get("author")
        .and_then(Value::as_array)
        .map(|values| values.iter().map(crossref_author).collect())
        .unwrap_or_default();
    record.metrics.cited_by_count = item
        .get("is-referenced-by-count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let retracted = ["update-to", "updated-by", "relation"].iter().any(|field| {
        item.get(*field)
            .and_then(|value| serde_json::to_string(value).ok())
            .is_some_and(|value| value.to_lowercase().contains("retract"))
    });
    record.flags.insert("is_retracted".to_owned(), retracted);
    if let Some(links) = item.get("link").and_then(Value::as_array) {
        for link in links {
            let content_type = string(link, "/content-type");
            let url = string(link, "/URL");
            if content_type.to_lowercase().contains("pdf") && !url.is_empty() {
                let version = string(link, "/content-version");
                let (license, license_is_open) = crossref_license(item, &version);
                record.fulltext_candidates.push(FullTextCandidate {
                    authorized: license_is_open || repository_url(&url),
                    url,
                    source: "crossref".to_owned(),
                    version,
                    license,
                    content_type,
                });
            }
        }
    }
    Ok(record)
}

fn crossref_license(item: &Value, content_version: &str) -> (String, bool) {
    let mut fallback = String::new();
    let now = Utc::now().timestamp_millis();
    for license in item
        .get("license")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let version = string(license, "/content-version");
        if !version.is_empty()
            && !content_version.is_empty()
            && !version.eq_ignore_ascii_case(content_version)
        {
            continue;
        }
        let url = string(license, "/URL");
        if fallback.is_empty() {
            fallback = url.clone();
        }
        let active = license
            .pointer("/start/timestamp")
            .and_then(Value::as_i64)
            .is_none_or(|timestamp| timestamp <= now);
        if active && is_open_license(&url) {
            return (url, true);
        }
    }
    (fallback, false)
}

fn arxiv_record(entry: &ArxivEntry) -> Result<WorkRecord> {
    let arxiv_id = normalize_arxiv(&entry.id).context("arXiv entry missing identifier")?;
    let mut record = WorkRecord::new("arxiv", &arxiv_id);
    record.ids.insert("arxiv".to_owned(), arxiv_id.clone());
    if let Some(doi) = entry.doi.as_deref().and_then(normalize_doi) {
        record.ids.insert("doi".to_owned(), doi);
    }
    record.title = normalize_space(&entry.title);
    record.abstract_text = normalize_space(&entry.summary);
    record.work_type = "preprint".to_owned();
    record.container_title = "arXiv".to_owned();
    record.url = format!("https://arxiv.org/abs/{arxiv_id}");
    record.issued = date_parts(entry.published.get(..10).unwrap_or(&entry.published));
    record.authors = entry
        .authors
        .iter()
        .map(|author| author_from_literal(&author.name))
        .collect();
    record.subjects = entry
        .categories
        .iter()
        .map(|category| category.term.clone())
        .collect();
    record.fulltext_candidates.push(FullTextCandidate {
        url: format!("https://arxiv.org/pdf/{arxiv_id}"),
        source: "arxiv".to_owned(),
        version: "submittedVersion".to_owned(),
        license: entry.license.clone().unwrap_or_default(),
        content_type: "application/pdf".to_owned(),
        authorized: true,
    });
    Ok(record)
}

fn merge_discovered(items: Vec<DiscoveredWork>) -> Vec<DiscoveredWork> {
    let mut works: Vec<DiscoveredWork> = Vec::new();
    let mut aliases: HashMap<String, usize> = HashMap::new();
    for item in items {
        let item_aliases = aliases_for(&item.record);
        if let Some(index) = item_aliases
            .iter()
            .find_map(|key| aliases.get(key))
            .copied()
        {
            let existing = &mut works[index];
            existing.record = existing.record.clone().merge(item.record);
            existing.raw_records.extend(item.raw_records);
            for alias in aliases_for(&existing.record) {
                aliases.insert(alias, index);
            }
        } else {
            let index = works.len();
            for alias in item_aliases {
                aliases.insert(alias, index);
            }
            works.push(item);
        }
    }
    works
}

fn aliases_for(record: &WorkRecord) -> Vec<String> {
    let mut aliases = vec![record.identity()];
    let mut has_persistent_id = false;
    if let Some(doi) = record.ids.get("doi").and_then(|value| normalize_doi(value)) {
        aliases.push(format!("doi:{doi}"));
        has_persistent_id = true;
    }
    if let Some(arxiv) = record
        .ids
        .get("arxiv")
        .and_then(|value| normalize_arxiv(value))
    {
        aliases.push(format!("arxiv:{}", arxiv.to_lowercase()));
        if let Some(base) = arxiv_base_id(&arxiv) {
            aliases.push(format!("arxiv:{}", base.to_lowercase()));
        }
        has_persistent_id = true;
    }
    if let Some(openalex) = record
        .ids
        .get("openalex")
        .and_then(|value| normalize_openalex(value))
    {
        aliases.push(format!("openalex:{}", openalex.to_lowercase()));
        has_persistent_id = true;
    }
    if !has_persistent_id {
        aliases.push(format!(
            "title:{}:{}",
            title_fingerprint(&record.title),
            record.year().unwrap_or(0)
        ));
    }
    aliases
}

async fn enrich_semantic_scholar(
    client: &Client,
    settings: &Settings,
    works: &mut [DiscoveredWork],
) -> Result<()> {
    let mut offset = 0;
    while offset < works.len() {
        let end = (offset + 100).min(works.len());
        let ids = works[offset..end]
            .iter()
            .map(|work| {
                if let Some(doi) = work.record.ids.get("doi") {
                    format!("DOI:{doi}")
                } else if let Some(arxiv) = work.record.ids.get("arxiv") {
                    format!("ARXIV:{arxiv}")
                } else {
                    work.record.identity()
                }
            })
            .collect::<Vec<_>>();
        let response = client
            .post("https://api.semanticscholar.org/graph/v1/paper/batch")
            .header("x-api-key", &settings.semantic_scholar_api_key)
            .query(&[(
                "fields",
                "paperId,externalIds,citationCount,influentialCitationCount,isOpenAccess,openAccessPdf",
            )])
            .json(&json!({ "ids": ids }))
            .send()
            .await?
            .error_for_status()?;
        let payload: Vec<Option<Value>> = response.json().await?;
        for (local, value) in payload.into_iter().enumerate() {
            let Some(value) = value else { continue };
            let work = &mut works[offset + local];
            work.record.metrics.cited_by_count = work.record.metrics.cited_by_count.max(
                value
                    .get("citationCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
            work.record.metrics.influential_citation_count = value
                .get("influentialCitationCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if let Some(id) = value.get("paperId").and_then(Value::as_str) {
                work.record.ids.insert("semantic_scholar".into(), id.into());
            }
            let pdf_url = string(&value, "/openAccessPdf/url");
            if !pdf_url.is_empty() {
                work.record.fulltext_candidates.push(FullTextCandidate {
                    url: pdf_url,
                    source: "semantic_scholar".into(),
                    version: String::new(),
                    license: string(&value, "/openAccessPdf/license"),
                    content_type: "application/pdf".into(),
                    authorized: value
                        .get("isOpenAccess")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
            work.raw_records.push(RawSourceRecord {
                source: "semantic_scholar".into(),
                source_id: string(&value, "/paperId"),
                retrieved_at: now(),
                raw: value,
            });
        }
        offset = end;
    }
    Ok(())
}

async fn retry_get(
    client: &Client,
    url: &str,
    params: &[(&str, String)],
) -> Result<reqwest::Response> {
    let mut delay = 1;
    for attempt in 0..5 {
        match client.get(url).query(params).send().await {
            Ok(response)
                if response.status() != StatusCode::TOO_MANY_REQUESTS
                    && !response.status().is_server_error() =>
            {
                return Ok(response);
            }
            Ok(response) => {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(delay);
                warn!(attempt, status = %response.status(), retry_after, "retrying request");
                sleep(Duration::from_secs(retry_after.min(60))).await;
            }
            Err(error) if attempt < 4 => {
                warn!(attempt, %error, "retrying failed request");
                sleep(Duration::from_secs(delay)).await;
            }
            Err(error) => return Err(error.into()),
        }
        delay = (delay * 2).min(30);
    }
    bail!("request exhausted retries: {url}")
}

fn openalex_author(value: &Value) -> Author {
    let literal = string(value, "/author/display_name");
    let (given, family) = split_name(&literal);
    let affiliations = value
        .get("institutions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("display_name").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut ids = BTreeMap::new();
    let openalex = string(value, "/author/id");
    if !openalex.is_empty() {
        ids.insert("openalex".to_owned(), openalex);
    }
    Author {
        given,
        family,
        literal,
        orcid: nonempty(string(value, "/author/orcid")),
        affiliations,
        ids,
    }
}

fn crossref_author(value: &Value) -> Author {
    let given = string(value, "/given");
    let family = string(value, "/family");
    let literal = normalize_space(&format!("{given} {family}"));
    let affiliations = value
        .get("affiliation")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Author {
        given,
        family,
        literal,
        orcid: nonempty(string(value, "/ORCID")),
        affiliations,
        ids: BTreeMap::new(),
    }
}

fn author_from_literal(literal: &str) -> Author {
    let literal = normalize_space(literal);
    let (given, family) = split_name(&literal);
    Author {
        given,
        family,
        literal,
        ..Author::default()
    }
}

fn split_name(literal: &str) -> (String, String) {
    let mut parts = literal.rsplitn(2, ' ');
    let family = parts.next().unwrap_or_default().to_owned();
    let given = parts.next().unwrap_or_default().to_owned();
    (given, family)
}

fn crossref_date(item: &Value) -> DateParts {
    for pointer in [
        "/published-print",
        "/published-online",
        "/issued",
        "/created",
    ] {
        if let Some(parts) = item
            .pointer(pointer)
            .and_then(|value| value.get("date-parts"))
            .and_then(Value::as_array)
        {
            let parsed = parts
                .iter()
                .filter_map(|outer| {
                    outer.as_array().map(|inner| {
                        inner
                            .iter()
                            .filter_map(|value| value.as_u64().map(|value| value as u32))
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();
            if !parsed.is_empty() {
                return DateParts {
                    date_parts: parsed,
                    raw: String::new(),
                };
            }
        }
    }
    DateParts::default()
}

fn date_parts(value: &str) -> DateParts {
    match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        Ok(date) => DateParts {
            date_parts: vec![vec![date.year() as u32, date.month(), date.day()]],
            raw: value.to_owned(),
        },
        Err(_) => DateParts {
            date_parts: Vec::new(),
            raw: value.to_owned(),
        },
    }
}

fn abstract_from_inverted(value: Option<&Value>) -> String {
    let Some(Value::Object(index)) = value else {
        return String::new();
    };
    let mut words = Vec::new();
    for (word, positions) in index {
        if let Some(positions) = positions.as_array() {
            for position in positions.iter().filter_map(Value::as_u64) {
                words.push((position, word.as_str()));
            }
        }
    }
    words.sort_by_key(|(position, _)| *position);
    normalize_space(
        &words
            .into_iter()
            .map(|(_, word)| word)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn work_type(value: &str) -> String {
    match value.to_lowercase().replace('_', "-").as_str() {
        "journal-article" | "article" => "article-journal",
        "proceedings-article" | "proceedings" => "paper-conference",
        "book-chapter" => "chapter",
        "posted-content" | "preprint" => "preprint",
        "dissertation" => "thesis",
        other if !other.is_empty() => other,
        _ => "article",
    }
    .to_owned()
}

fn page_range(first: &str, last: &str) -> String {
    match (first.is_empty(), last.is_empty(), first == last) {
        (false, false, false) => format!("{first}-{last}"),
        (false, _, _) => first.to_owned(),
        (_, false, _) => last.to_owned(),
        _ => String::new(),
    }
}

fn string(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(|value| match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn first_string(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn is_open_license(value: &str) -> bool {
    let value = value.to_lowercase();
    value.contains("creativecommons.org/licenses/")
        || value.contains("creativecommons.org/publicdomain/")
        || value.contains("opendatacommons.org/licenses/")
        || value.contains("rightsstatements.org/vocab/noc-")
}

fn repository_url(value: &str) -> bool {
    let host = Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(ToOwned::to_owned))
        .unwrap_or_default()
        .to_lowercase();
    [
        "arxiv.org",
        "ncbi.nlm.nih.gov",
        "europepmc.org",
        "zenodo.org",
        "hal.science",
    ]
    .iter()
    .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
}

fn in_date_range(record: &WorkRecord, plan: &ResearchPlan) -> bool {
    let Some(year) = record.year() else {
        return true;
    };
    let lower = plan
        .date_from
        .as_deref()
        .and_then(|value| value.get(..4))
        .and_then(|value| value.parse::<u32>().ok());
    let upper = plan
        .date_to
        .as_deref()
        .and_then(|value| value.get(..4))
        .and_then(|value| value.parse::<u32>().ok());
    lower.is_none_or(|lower| year >= lower) && upper.is_none_or(|upper| year <= upper)
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
struct ArxivFeed {
    #[serde(rename = "entry", default)]
    entries: Vec<ArxivEntry>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
struct ArxivEntry {
    id: String,
    title: String,
    summary: String,
    published: String,
    #[serde(rename = "author", default)]
    authors: Vec<ArxivAuthor>,
    #[serde(rename = "category", default)]
    categories: Vec<ArxivCategory>,
    #[serde(rename = "doi", default)]
    doi: Option<String>,
    #[serde(rename = "license", default)]
    license: Option<String>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
struct ArxivAuthor {
    name: String,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
struct ArxivCategory {
    #[serde(rename = "@term", default)]
    term: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstructs_openalex_abstract() {
        let value = json!({"world": [1], "Hello": [0]});
        assert_eq!(abstract_from_inverted(Some(&value)), "Hello world");
    }

    #[test]
    fn only_authorizes_known_open_fulltext() {
        assert!(is_open_license(
            "https://creativecommons.org/licenses/by/4.0/"
        ));
        assert!(repository_url("https://arxiv.org/pdf/2401.1"));
        assert!(!repository_url("https://evilarxiv.org/paper.pdf"));
        assert!(!repository_url("https://publisher.example/article.pdf"));
    }

    #[test]
    fn parses_namespaced_arxiv_atom_metadata() {
        let xml = r#"
            <feed xmlns="http://www.w3.org/2005/Atom"
                  xmlns:arxiv="http://arxiv.org/schemas/atom">
              <entry>
                <id>https://arxiv.org/abs/2401.00001v2</id>
                <title>Visual scholarly retrieval</title>
                <summary>A complete abstract.</summary>
                <published>2024-01-02T00:00:00Z</published>
                <author><name>Ada Chen</name></author>
                <category term="cs.IR"/>
                <arxiv:doi>10.1000/example</arxiv:doi>
                <arxiv:license>https://creativecommons.org/licenses/by/4.0/</arxiv:license>
              </entry>
            </feed>
        "#;
        let feed: ArxivFeed = from_str(xml).unwrap();
        assert_eq!(feed.entries.len(), 1);
        let record = arxiv_record(&feed.entries[0]).unwrap();
        assert_eq!(record.ids["arxiv"], "2401.00001v2");
        assert_eq!(record.ids["doi"], "10.1000/example");
        assert_eq!(record.authors[0].family, "Chen");
        assert_eq!(record.subjects, vec!["cs.IR"]);
    }

    #[test]
    fn finds_versioned_arxiv_entry_for_unversioned_request() {
        let xml = r#"
            <feed xmlns="http://www.w3.org/2005/Atom">
              <entry>
                <id>https://arxiv.org/abs/2401.00001v2</id>
                <title>Visual scholarly retrieval</title>
                <summary>A complete abstract.</summary>
                <published>2024-01-02T00:00:00Z</published>
              </entry>
            </feed>
        "#;
        let feed: ArxivFeed = from_str(xml).unwrap();
        let entry = feed
            .entries
            .iter()
            .find(|entry| arxiv_ids_match("2401.00001", &entry.id))
            .unwrap();
        assert_eq!(normalize_arxiv(&entry.id), Some("2401.00001v2".to_owned()));
        assert!(
            !feed
                .entries
                .iter()
                .any(|entry| arxiv_ids_match("2401.00001v1", &entry.id))
        );
    }

    #[test]
    fn maps_complete_crossref_citation_and_retraction_metadata() {
        let item = json!({
            "DOI": "10.1000/example",
            "title": ["A complete scholarly record"],
            "abstract": "<jats:p>Evidence abstract.</jats:p>",
            "type": "journal-article",
            "container-title": ["Journal of Evidence"],
            "publisher": "Scholarly Publisher",
            "volume": "12",
            "issue": "3",
            "page": "100-120",
            "language": "en",
            "URL": "https://doi.org/10.1000/example",
            "published-online": {"date-parts": [[2025, 4, 2]]},
            "author": [{
                "given": "Ada",
                "family": "Chen",
                "ORCID": "https://orcid.org/0000-0000-0000-0001",
                "affiliation": [{"name": "Example University"}]
            }],
            "ISSN": ["1234-5678"],
            "subject": ["Information Retrieval"],
            "is-referenced-by-count": 42,
            "updated-by": [{"type": "retraction", "DOI": "10.1000/retraction"}],
            "license": [{"URL": "https://creativecommons.org/licenses/by/4.0/"}],
            "link": [{
                "URL": "https://publisher.example/paper.pdf",
                "content-type": "application/pdf",
                "content-version": "vor"
            }]
        });
        let record = crossref_record(&item).unwrap();
        assert_eq!(record.ids["doi"], "10.1000/example");
        assert_eq!(record.year(), Some(2025));
        assert_eq!(record.authors[0].family, "Chen");
        assert_eq!(record.container_title, "Journal of Evidence");
        assert!(record.flags["is_retracted"]);
        assert!(record.fulltext_candidates[0].authorized);

        let mut future = item;
        future["license"][0]["start"] = json!({"timestamp": 4102444800000_i64});
        let record = crossref_record(&future).unwrap();
        assert!(!record.fulltext_candidates[0].authorized);
    }

    #[test]
    fn maps_openalex_quality_and_open_access_signals() {
        let item = json!({
            "id": "https://openalex.org/W123",
            "ids": {"doi": "https://doi.org/10.1000/openalex"},
            "title": "Visual document retrieval",
            "abstract_inverted_index": {"Visual": [0], "retrieval": [1]},
            "type": "article",
            "language": "en",
            "publication_date": "2026-01-02",
            "doi": "https://doi.org/10.1000/openalex",
            "primary_location": {
                "landing_page_url": "https://publisher.example/article",
                "source": {
                    "display_name": "Journal",
                    "host_organization_name": "Publisher",
                    "issn": ["1234-5678"]
                }
            },
            "authorships": [{
                "author": {"display_name": "Ada Chen", "id": "https://openalex.org/A1"},
                "institutions": [{"display_name": "Example University"}]
            }],
            "topics": [{"display_name": "Information Retrieval"}],
            "keywords": [{"display_name": "vision retrieval"}],
            "cited_by_count": 10,
            "fwci": 2.1,
            "citation_normalized_percentile": {"value": 0.95},
            "is_retracted": false,
            "is_paratext": false,
            "open_access": {"is_oa": true},
            "locations": [{
                "is_oa": true,
                "pdf_url": "https://repository.example/paper.pdf",
                "version": "acceptedVersion",
                "license": "cc-by"
            }]
        });
        let record = openalex_record(&item).unwrap();
        assert_eq!(record.abstract_text, "Visual retrieval");
        assert_eq!(record.metrics.fwci, Some(2.1));
        assert_eq!(record.metrics.citation_percentile, Some(0.95));
        assert_eq!(record.issn, vec!["1234-5678"]);
        assert!(record.fulltext_candidates[0].authorized);
    }

    #[test]
    fn never_merges_distinct_dois_by_title_alone() {
        let mut first = WorkRecord::new("crossref", "10.1000/first");
        first.ids.insert("doi".into(), "10.1000/first".into());
        first.title = "Shared title".into();
        first.issued.date_parts = vec![vec![2025]];
        let mut second = WorkRecord::new("crossref", "10.1000/second");
        second.ids.insert("doi".into(), "10.1000/second".into());
        second.title = "Shared title".into();
        second.issued.date_parts = vec![vec![2025]];
        let merged = merge_discovered(vec![
            DiscoveredWork {
                record: first,
                raw_records: Vec::new(),
            },
            DiscoveredWork {
                record: second,
                raw_records: Vec::new(),
            },
        ]);
        assert_eq!(merged.len(), 2);
    }
}
