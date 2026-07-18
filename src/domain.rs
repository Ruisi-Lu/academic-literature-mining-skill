use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util::{
    arxiv_base_id, normalize_arxiv, normalize_doi, normalize_openalex, normalize_space,
    title_fingerprint, unique,
};

pub const SCHEMA_VERSION: &str = "1.0";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchPlan {
    pub research_question: String,
    pub queries: Vec<String>,
    #[serde(default)]
    pub inclusion_criteria: Vec<String>,
    #[serde(default)]
    pub exclusion_criteria: Vec<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default = "default_sources")]
    pub sources: Vec<String>,
    #[serde(default)]
    pub include_preprints: bool,
    #[serde(default = "default_target")]
    pub target_papers: usize,
    #[serde(default = "default_quality")]
    pub min_quality_score: f64,
    #[serde(default = "default_relevance")]
    pub min_relevance_score: f64,
}

impl ResearchPlan {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let mut plan: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse research plan {}", path.display()))?;
        plan.research_question = normalize_space(&plan.research_question);
        plan.queries = unique(plan.queries);
        plan.inclusion_criteria = unique(plan.inclusion_criteria);
        plan.exclusion_criteria = unique(plan.exclusion_criteria);
        plan.languages = unique(plan.languages);
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<()> {
        if self.research_question.is_empty() {
            bail!("research_question is required");
        }
        if self.queries.is_empty() {
            bail!("queries must contain at least one query");
        }
        let supported = ["openalex", "crossref", "arxiv"];
        for source in &self.sources {
            if !supported.contains(&source.as_str()) {
                bail!("unsupported discovery source: {source}");
            }
        }
        for (name, value) in [
            ("date_from", self.date_from.as_deref()),
            ("date_to", self.date_to.as_deref()),
        ] {
            if let Some(value) = value {
                NaiveDate::parse_from_str(value, "%Y-%m-%d")
                    .with_context(|| format!("{name} must use YYYY-MM-DD"))?;
            }
        }
        if self.target_papers == 0 {
            bail!("target_papers must be positive");
        }
        if let (Some(from), Some(to)) = (&self.date_from, &self.date_to)
            && from > to
        {
            bail!("date_from must not be later than date_to");
        }
        if !(0.0..=100.0).contains(&self.min_quality_score) {
            bail!("min_quality_score must be between 0 and 100");
        }
        if !(0.0..=1.0).contains(&self.min_relevance_score) {
            bail!("min_relevance_score must be between 0 and 1");
        }
        Ok(())
    }

    pub fn screening_query(&self) -> String {
        let mut sections = vec![self.research_question.clone()];
        if !self.inclusion_criteria.is_empty() {
            sections.push(format!("Include: {}", self.inclusion_criteria.join("; ")));
        }
        if !self.exclusion_criteria.is_empty() {
            sections.push(format!("Exclude: {}", self.exclusion_criteria.join("; ")));
        }
        sections.join("\n")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Author {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub given: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub family: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub literal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orcid: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affiliations: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ids: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DateParts {
    #[serde(rename = "date-parts", default)]
    pub date_parts: Vec<Vec<u32>>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub raw: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FullTextCandidate {
    pub url: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub authorized: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Metrics {
    #[serde(default)]
    pub cited_by_count: u64,
    #[serde(default)]
    pub influential_citation_count: u64,
    pub fwci: Option<f64>,
    pub citation_percentile: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct QualityAssessment {
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub relevance_score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relevance_logit: Option<f64>,
    #[serde(default)]
    pub priority_score: f64,
    #[serde(default)]
    pub accepted: bool,
    #[serde(default)]
    pub signals: Vec<String>,
    #[serde(default)]
    pub rejection_reasons: Vec<String>,
    #[serde(default)]
    pub screened_at: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ProvenanceSource {
    pub source: String,
    pub source_id: String,
    #[serde(default)]
    pub retrieved_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkRecord {
    #[serde(default = "schema_version")]
    pub schema_version: String,
    #[serde(default)]
    pub ids: BTreeMap<String, String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub abstract_text: String,
    #[serde(default)]
    pub authors: Vec<Author>,
    #[serde(default)]
    pub issued: DateParts,
    #[serde(default = "default_work_type")]
    pub work_type: String,
    #[serde(default)]
    pub container_title: String,
    #[serde(default)]
    pub publisher: String,
    #[serde(default)]
    pub volume: String,
    #[serde(default)]
    pub issue: String,
    #[serde(default)]
    pub page: String,
    #[serde(default)]
    pub article_number: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub subjects: Vec<String>,
    #[serde(default)]
    pub issn: Vec<String>,
    #[serde(default)]
    pub isbn: Vec<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub fulltext_candidates: Vec<FullTextCandidate>,
    #[serde(default)]
    pub metrics: Metrics,
    #[serde(default)]
    pub flags: BTreeMap<String, bool>,
    #[serde(default)]
    pub quality: QualityAssessment,
    #[serde(default)]
    pub provenance: Vec<ProvenanceSource>,
}

impl WorkRecord {
    pub fn new(source: &str, source_id: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_owned(),
            ids: BTreeMap::new(),
            title: String::new(),
            abstract_text: String::new(),
            authors: Vec::new(),
            issued: DateParts::default(),
            work_type: default_work_type(),
            container_title: String::new(),
            publisher: String::new(),
            volume: String::new(),
            issue: String::new(),
            page: String::new(),
            article_number: String::new(),
            language: String::new(),
            keywords: Vec::new(),
            subjects: Vec::new(),
            issn: Vec::new(),
            isbn: Vec::new(),
            url: String::new(),
            fulltext_candidates: Vec::new(),
            metrics: Metrics::default(),
            flags: BTreeMap::new(),
            quality: QualityAssessment::default(),
            provenance: vec![ProvenanceSource {
                source: source.to_owned(),
                source_id: source_id.to_owned(),
                retrieved_at: crate::util::now(),
            }],
        }
    }

    pub fn year(&self) -> Option<u32> {
        self.issued
            .date_parts
            .first()
            .and_then(|parts| parts.first())
            .copied()
    }

    pub fn identity(&self) -> String {
        if let Some(doi) = self.ids.get("doi").and_then(|value| normalize_doi(value)) {
            return format!("doi:{doi}");
        }
        if let Some(arxiv) = self.ids.get("arxiv").and_then(|value| arxiv_base_id(value)) {
            return format!("arxiv:{}", arxiv.to_lowercase());
        }
        if let Some(openalex) = self.ids.get("openalex") {
            return format!(
                "openalex:{}",
                openalex
                    .rsplit('/')
                    .next()
                    .unwrap_or(openalex)
                    .to_lowercase()
            );
        }
        let author = self
            .authors
            .first()
            .map(|value| value.family.to_lowercase())
            .unwrap_or_default();
        format!(
            "title:{}:{}:{}",
            title_fingerprint(&self.title),
            self.year()
                .map_or_else(|| "nd".to_owned(), |year| year.to_string()),
            author
        )
    }

    pub fn screening_passage(&self) -> String {
        format!(
            "Title: {}\nYear: {}\nType: {}\nVenue: {}\nAbstract: {}",
            self.title,
            self.year()
                .map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
            self.work_type,
            self.container_title,
            self.abstract_text
        )
    }

    pub fn merge(mut self, other: Self) -> Self {
        prefer_longer(&mut self.title, other.title);
        prefer_longer(&mut self.abstract_text, other.abstract_text);
        prefer_if_empty(&mut self.container_title, other.container_title);
        prefer_if_empty(&mut self.publisher, other.publisher);
        prefer_if_empty(&mut self.volume, other.volume);
        prefer_if_empty(&mut self.issue, other.issue);
        prefer_if_empty(&mut self.page, other.page);
        prefer_if_empty(&mut self.article_number, other.article_number);
        prefer_if_empty(&mut self.language, other.language);
        prefer_if_empty(&mut self.url, other.url);
        if self.issued.date_parts.is_empty() && !other.issued.date_parts.is_empty() {
            self.issued = other.issued;
        }
        merge_authors(&mut self.authors, other.authors);
        self.ids.extend(other.ids);
        self.metrics.cited_by_count = self
            .metrics
            .cited_by_count
            .max(other.metrics.cited_by_count);
        self.metrics.influential_citation_count = self
            .metrics
            .influential_citation_count
            .max(other.metrics.influential_citation_count);
        self.metrics.fwci = max_option(self.metrics.fwci, other.metrics.fwci);
        self.metrics.citation_percentile = max_option(
            self.metrics.citation_percentile,
            other.metrics.citation_percentile,
        );
        for (name, value) in other.flags {
            self.flags
                .entry(name)
                .and_modify(|existing| *existing |= value)
                .or_insert(value);
        }
        self.keywords = unique(self.keywords.into_iter().chain(other.keywords));
        self.subjects = unique(self.subjects.into_iter().chain(other.subjects));
        self.issn = unique(self.issn.into_iter().chain(other.issn));
        self.isbn = unique(self.isbn.into_iter().chain(other.isbn));
        merge_fulltext(&mut self.fulltext_candidates, other.fulltext_candidates);
        merge_provenance(&mut self.provenance, other.provenance);
        self
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RawSourceRecord {
    pub source: String,
    pub source_id: String,
    pub retrieved_at: String,
    pub raw: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PageRecord {
    pub page_id: String,
    pub work_id: String,
    pub page_number: u32,
    pub image_path: String,
    pub image_sha256: String,
    pub width: u32,
    pub height: u32,
    pub indexed_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PdfArtifact {
    pub url: String,
    pub path: String,
    pub sha256: String,
    pub license: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCandidate {
    pub task_id: String,
    pub query: String,
    pub source: String,
    pub title: String,
    #[serde(default)]
    pub doi: Option<String>,
    #[serde(default)]
    pub arxiv_id: Option<String>,
    #[serde(default)]
    pub openalex_id: Option<String>,
    #[serde(default)]
    pub landing_url: Option<String>,
    #[serde(default)]
    pub evidence_urls: Vec<String>,
    pub discovered_at: String,
}

impl AgentCandidate {
    pub fn validate(&self) -> Result<()> {
        if self.task_id.trim().is_empty()
            || self.query.trim().is_empty()
            || self.source.trim().is_empty()
            || self.title.trim().is_empty()
        {
            bail!("agent candidate requires task_id, query, source, and title");
        }
        if self.doi.as_deref().and_then(normalize_doi).is_none()
            && self.arxiv_id.as_deref().and_then(normalize_arxiv).is_none()
            && self
                .openalex_id
                .as_deref()
                .and_then(normalize_openalex)
                .is_none()
        {
            bail!("agent candidate requires DOI, arXiv ID, or OpenAlex ID");
        }
        if self.evidence_urls.is_empty() {
            bail!("agent candidate requires at least one evidence URL");
        }
        for value in self
            .evidence_urls
            .iter()
            .map(String::as_str)
            .chain(self.landing_url.as_deref())
        {
            let url = url::Url::parse(value).context("agent candidate contains an invalid URL")?;
            if !matches!(url.scheme(), "http" | "https") {
                bail!("agent candidate URLs must use HTTP(S)");
            }
        }
        DateTime::parse_from_rfc3339(&self.discovered_at)
            .context("agent candidate discovered_at must be RFC 3339")?;
        Ok(())
    }
}

fn schema_version() -> String {
    SCHEMA_VERSION.to_owned()
}

fn default_sources() -> Vec<String> {
    vec![
        "openalex".to_owned(),
        "crossref".to_owned(),
        "arxiv".to_owned(),
    ]
}

fn default_target() -> usize {
    200
}

fn default_quality() -> f64 {
    60.0
}

fn default_relevance() -> f64 {
    0.0
}

fn default_work_type() -> String {
    "article".to_owned()
}

fn prefer_if_empty(target: &mut String, candidate: String) {
    if target.is_empty() && !candidate.is_empty() {
        *target = candidate;
    }
}

fn prefer_longer(target: &mut String, candidate: String) {
    if candidate.len() > target.len() {
        *target = candidate;
    }
}

fn max_option(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn merge_fulltext(target: &mut Vec<FullTextCandidate>, candidates: Vec<FullTextCandidate>) {
    for candidate in candidates {
        if let Some(existing) = target.iter_mut().find(|item| item.url == candidate.url) {
            if existing.license.is_empty() {
                existing.license = candidate.license;
            }
            existing.authorized |= candidate.authorized;
        } else {
            target.push(candidate);
        }
    }
}

fn merge_authors(target: &mut Vec<Author>, candidates: Vec<Author>) {
    for candidate in candidates {
        if let Some(existing) = target
            .iter_mut()
            .find(|author| authors_match(author, &candidate))
        {
            prefer_if_empty(&mut existing.given, candidate.given);
            prefer_if_empty(&mut existing.family, candidate.family);
            prefer_if_empty(&mut existing.literal, candidate.literal);
            if existing.orcid.is_none() {
                existing.orcid = candidate.orcid;
            }
            existing.affiliations = unique(
                existing
                    .affiliations
                    .drain(..)
                    .chain(candidate.affiliations),
            );
            existing.ids.extend(candidate.ids);
        } else {
            target.push(candidate);
        }
    }
}

fn authors_match(left: &Author, right: &Author) -> bool {
    if let (Some(left), Some(right)) = (&left.orcid, &right.orcid)
        && left.trim().eq_ignore_ascii_case(right.trim())
    {
        return true;
    }
    let name = |author: &Author| {
        if !author.given.is_empty() || !author.family.is_empty() {
            title_fingerprint(&format!("{} {}", author.given, author.family))
        } else {
            title_fingerprint(&author.literal)
        }
    };
    let left = name(left);
    !left.is_empty() && left == name(right)
}

fn merge_provenance(target: &mut Vec<ProvenanceSource>, candidates: Vec<ProvenanceSource>) {
    for candidate in candidates {
        if !target
            .iter()
            .any(|item| item.source == candidate.source && item.source_id == candidate.source_id)
        {
            target.push(candidate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_doi_as_identity() {
        let mut record = WorkRecord::new("test", "one");
        record
            .ids
            .insert("doi".into(), "https://doi.org/10.1000/ABC".into());
        assert_eq!(record.identity(), "doi:10.1000/abc");
    }

    #[test]
    fn uses_unversioned_arxiv_id_as_identity() {
        let mut record = WorkRecord::new("arxiv", "2401.00001v2");
        record.ids.insert("arxiv".into(), "2401.00001v2".into());
        assert_eq!(record.identity(), "arxiv:2401.00001");
        assert_eq!(record.ids["arxiv"], "2401.00001v2");
    }

    #[test]
    fn merging_keeps_richer_metadata() {
        let mut left = WorkRecord::new("openalex", "W1");
        left.title = "Short".into();
        left.authors.push(Author {
            given: "Ada".into(),
            family: "Chen".into(),
            literal: "Ada Chen".into(),
            affiliations: vec!["University A".into()],
            ..Author::default()
        });
        let mut right = WorkRecord::new("crossref", "10.1000/x");
        right.title = "A much more complete scholarly title".into();
        right.metrics.cited_by_count = 20;
        right.authors.push(Author {
            given: "Ada".into(),
            family: "Chen".into(),
            literal: "Ada Chen".into(),
            orcid: Some("https://orcid.org/0000-0000-0000-0001".into()),
            affiliations: vec!["Institute B".into()],
            ..Author::default()
        });
        left.flags.insert("is_retracted".into(), true);
        right.flags.insert("is_retracted".into(), false);
        let merged = left.merge(right);
        assert_eq!(merged.metrics.cited_by_count, 20);
        assert_eq!(merged.provenance.len(), 2);
        assert!(merged.flags["is_retracted"]);
        assert_eq!(merged.authors.len(), 1);
        assert!(merged.authors[0].orcid.is_some());
        assert_eq!(merged.authors[0].affiliations.len(), 2);
    }

    #[test]
    fn rejects_agent_candidates_without_a_valid_identifier() {
        let candidate = AgentCandidate {
            task_id: "task".into(),
            query: "query".into(),
            source: "openalex".into(),
            title: "title".into(),
            doi: None,
            arxiv_id: None,
            openalex_id: Some("not-openalex".into()),
            landing_url: None,
            evidence_urls: vec!["https://example.org/record".into()],
            discovered_at: "2026-01-01T00:00:00Z".into(),
        };
        assert!(candidate.validate().is_err());
    }

    #[test]
    fn defaults_to_rank_only_relevance_screening() {
        let plan: ResearchPlan = serde_json::from_value(serde_json::json!({
            "research_question": "question",
            "queries": ["query"],
            "date_from": null,
            "date_to": null
        }))
        .unwrap();
        assert_eq!(plan.min_relevance_score, 0.0);
    }
}
