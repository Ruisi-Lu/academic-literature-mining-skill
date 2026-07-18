use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::domain::{Author, WorkRecord};
use crate::util::{atomic_json, normalize_arxiv, normalize_doi, normalize_openalex, safe_slug};

#[derive(Debug, Serialize)]
pub struct CitationAudit {
    pub total_records: usize,
    pub complete_records: usize,
    pub incomplete_records: Vec<IncompleteCitation>,
}

#[derive(Debug, Serialize)]
pub struct IncompleteCitation {
    pub work_id: String,
    pub missing_fields: Vec<String>,
}

pub fn csl_json(record: &WorkRecord, citation_key: &str) -> Value {
    let authors = record.authors.iter().map(csl_author).collect::<Vec<_>>();
    let mut value = Map::new();
    value.insert("id".into(), Value::String(citation_key.to_owned()));
    value.insert(
        "type".into(),
        Value::String(csl_type(&record.work_type).into()),
    );
    value.insert("title".into(), Value::String(record.title.clone()));
    value.insert("author".into(), Value::Array(authors));
    value.insert(
        "issued".into(),
        serde_json::to_value(&record.issued).unwrap_or_else(|_| json!({"date-parts": []})),
    );
    insert(&mut value, "container-title", &record.container_title);
    insert(&mut value, "publisher", &record.publisher);
    insert(&mut value, "volume", &record.volume);
    insert(&mut value, "issue", &record.issue);
    insert(&mut value, "page", &record.page);
    insert(&mut value, "number", &record.article_number);
    insert(&mut value, "language", &record.language);
    insert(&mut value, "URL", &record.url);
    if let Some(doi) = record.ids.get("doi").and_then(|value| normalize_doi(value)) {
        value.insert("DOI".into(), Value::String(doi));
    }
    if !record.issn.is_empty() {
        value.insert("ISSN".into(), Value::String(record.issn.join(", ")));
    }
    if !record.isbn.is_empty() {
        value.insert("ISBN".into(), Value::String(record.isbn.join(", ")));
    }
    if !record.abstract_text.is_empty() {
        value.insert(
            "abstract".into(),
            Value::String(record.abstract_text.clone()),
        );
    }
    if !record.keywords.is_empty() {
        value.insert("keyword".into(), Value::String(record.keywords.join(", ")));
    }
    if let Some(arxiv) = record.ids.get("arxiv") {
        value.insert("archive".into(), Value::String("arXiv".to_owned()));
        value.insert("archive_location".into(), Value::String(arxiv.clone()));
    }
    Value::Object(value)
}

pub fn export_library(workspace: &Path, works: &[(String, WorkRecord)]) -> Result<CitationAudit> {
    let export_dir = workspace.join("exports");
    fs::create_dir_all(&export_dir)?;
    let mut used_keys = HashSet::new();
    let mut csl_records = Vec::with_capacity(works.len());
    let mut bibtex = String::new();
    let mut ris = String::new();
    let records_path = export_dir.join("records.jsonl");
    let mut records_file = fs::File::create(&records_path)?;

    for (work_id, record) in works {
        let key = unique_key(record, &mut used_keys);
        csl_records.push(csl_json(record, &key));
        bibtex.push_str(&bibtex_entry(record, &key));
        bibtex.push('\n');
        ris.push_str(&ris_entry(record));
        ris.push('\n');
        serde_json::to_writer(
            &mut records_file,
            &json!({
                "work_id": work_id,
                "record": record,
                "citation_key": key
            }),
        )?;
        records_file.write_all(b"\n")?;
    }
    records_file.sync_all()?;
    atomic_json(&export_dir.join("library.csl.json"), &csl_records)?;
    fs::write(export_dir.join("library.bib"), bibtex)?;
    fs::write(export_dir.join("library.ris"), ris)?;
    let audit = audit(works);
    atomic_json(&export_dir.join("citation-audit.json"), &audit)?;
    Ok(audit)
}

pub fn audit(works: &[(String, WorkRecord)]) -> CitationAudit {
    let mut incomplete = Vec::new();
    for (work_id, record) in works {
        let mut missing = Vec::new();
        if record.title.is_empty() {
            missing.push("title".to_owned());
        }
        if !record.authors.iter().any(|author| {
            !author.literal.trim().is_empty()
                || !author.family.trim().is_empty()
                || !author.given.trim().is_empty()
        }) {
            missing.push("author".to_owned());
        }
        if record.year().is_none() {
            missing.push("issued".to_owned());
        }
        if record.work_type == "article-journal" && record.container_title.is_empty() {
            missing.push("container-title".to_owned());
        }
        if record
            .ids
            .get("doi")
            .and_then(|value| normalize_doi(value))
            .is_none()
            && record
                .ids
                .get("arxiv")
                .and_then(|value| normalize_arxiv(value))
                .is_none()
            && record
                .ids
                .get("openalex")
                .and_then(|value| normalize_openalex(value))
                .is_none()
        {
            missing.push("persistent-identifier".to_owned());
        }
        if !missing.is_empty() {
            incomplete.push(IncompleteCitation {
                work_id: work_id.clone(),
                missing_fields: missing,
            });
        }
    }
    CitationAudit {
        total_records: works.len(),
        complete_records: works.len() - incomplete.len(),
        incomplete_records: incomplete,
    }
}

fn csl_author(author: &Author) -> Value {
    let mut value = Map::new();
    if !author.family.is_empty() || !author.given.is_empty() {
        insert(&mut value, "family", &author.family);
        insert(&mut value, "given", &author.given);
    } else {
        insert(&mut value, "literal", &author.literal);
    }
    if let Some(orcid) = &author.orcid {
        insert(&mut value, "ORCID", orcid);
    }
    Value::Object(value)
}

fn csl_type(value: &str) -> &'static str {
    match value {
        "paper-conference" => "paper-conference",
        "chapter" => "chapter",
        "book" => "book",
        "thesis" => "thesis",
        "report" => "report",
        "dataset" => "dataset",
        "preprint" => "article",
        _ => "article-journal",
    }
}

fn insert(map: &mut Map<String, Value>, key: &str, value: &str) {
    if !value.is_empty() {
        map.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn unique_key(record: &WorkRecord, used: &mut HashSet<String>) -> String {
    let family = record
        .authors
        .first()
        .map(|author| {
            if author.family.is_empty() {
                &author.literal
            } else {
                &author.family
            }
        })
        .cloned()
        .unwrap_or_else(|| "anonymous".to_owned());
    let word = record
        .title
        .split_whitespace()
        .find(|word| word.chars().any(char::is_alphabetic))
        .unwrap_or("work");
    let base = format!(
        "{}{}{}",
        safe_slug(&family, "anonymous").replace('-', ""),
        record
            .year()
            .map_or_else(|| "nd".into(), |year| year.to_string()),
        safe_slug(word, "work").replace('-', "")
    );
    let mut key = base.clone();
    let mut suffix = 2_u64;
    while !used.insert(key.clone()) {
        key = format!("{base}{suffix}");
        suffix += 1;
    }
    key
}

fn bibtex_entry(record: &WorkRecord, key: &str) -> String {
    let entry_type = match record.work_type.as_str() {
        "paper-conference" => "inproceedings",
        "chapter" => "incollection",
        "book" => "book",
        "thesis" => "phdthesis",
        "report" => "techreport",
        _ => "article",
    };
    let authors = record
        .authors
        .iter()
        .map(|author| {
            if !author.family.is_empty() {
                format!("{}, {}", author.family, author.given)
            } else {
                format!("{{{}}}", author.literal)
            }
        })
        .collect::<Vec<_>>()
        .join(" and ");
    let mut fields = BTreeMap::new();
    fields.insert("title", record.title.clone());
    fields.insert("author", authors);
    fields.insert(
        "year",
        record
            .year()
            .map_or_else(String::new, |value| value.to_string()),
    );
    let container_field = match record.work_type.as_str() {
        "paper-conference" | "chapter" => "booktitle",
        _ => "journal",
    };
    fields.insert(container_field, record.container_title.clone());
    fields.insert("publisher", record.publisher.clone());
    fields.insert("volume", record.volume.clone());
    fields.insert("number", record.issue.clone());
    fields.insert("pages", record.page.clone());
    fields.insert("eid", record.article_number.clone());
    fields.insert("url", record.url.clone());
    fields.insert("language", record.language.clone());
    fields.insert("issn", record.issn.join(", "));
    fields.insert("isbn", record.isbn.join(", "));
    fields.insert("keywords", record.keywords.join(", "));
    fields.insert("abstract", record.abstract_text.clone());
    fields.insert(
        "doi",
        record
            .ids
            .get("doi")
            .and_then(|value| normalize_doi(value))
            .unwrap_or_default(),
    );
    if let Some(arxiv) = record.ids.get("arxiv") {
        fields.insert("eprint", arxiv.clone());
        fields.insert("archiveprefix", "arXiv".to_owned());
    }
    let body = fields
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| format!("  {name} = {{{}}}", bibtex_escape(&value)))
        .collect::<Vec<_>>()
        .join(",\n");
    format!("@{entry_type}{{{key},\n{body}\n}}\n")
}

fn bibtex_escape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => output.push_str(r"\textbackslash{}"),
            '{' => output.push_str(r"\{"),
            '}' => output.push_str(r"\}"),
            '&' => output.push_str(r"\&"),
            '%' => output.push_str(r"\%"),
            '#' => output.push_str(r"\#"),
            '_' => output.push_str(r"\_"),
            _ => output.push(character),
        }
    }
    output
}

fn ris_entry(record: &WorkRecord) -> String {
    let ris_type = match record.work_type.as_str() {
        "paper-conference" => "CPAPER",
        "book" => "BOOK",
        "chapter" => "CHAP",
        "thesis" => "THES",
        "report" => "RPRT",
        _ => "JOUR",
    };
    let mut lines = vec![
        format!("TY  - {ris_type}"),
        format!("TI  - {}", record.title),
    ];
    for author in &record.authors {
        let name = if author.family.is_empty() {
            author.literal.clone()
        } else {
            format!("{}, {}", author.family, author.given)
        };
        lines.push(format!("AU  - {name}"));
    }
    if let Some(year) = record.year() {
        lines.push(format!("PY  - {year}"));
    }
    for (tag, value) in [
        ("JO", record.container_title.as_str()),
        ("VL", record.volume.as_str()),
        ("IS", record.issue.as_str()),
        ("PB", record.publisher.as_str()),
        ("UR", record.url.as_str()),
        ("LA", record.language.as_str()),
        ("AB", record.abstract_text.as_str()),
        ("M3", record.article_number.as_str()),
    ] {
        if !value.is_empty() {
            lines.push(format!("{tag}  - {value}"));
        }
    }
    if let Some((start, end)) = split_page_range(&record.page) {
        lines.push(format!("SP  - {start}"));
        if let Some(end) = end {
            lines.push(format!("EP  - {end}"));
        }
    }
    for serial in record.issn.iter().chain(&record.isbn) {
        lines.push(format!("SN  - {serial}"));
    }
    for keyword in &record.keywords {
        lines.push(format!("KW  - {keyword}"));
    }
    if let Some(doi) = record.ids.get("doi").and_then(|value| normalize_doi(value)) {
        lines.push(format!("DO  - {doi}"));
    }
    if let Some(arxiv) = record.ids.get("arxiv") {
        lines.push(format!("AN  - arXiv:{arxiv}"));
    }
    lines.push("ER  - ".to_owned());
    lines.join("\n")
}

fn split_page_range(value: &str) -> Option<(&str, Option<&str>)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some((start, end)) = value.split_once(['-', '–', '—']) {
        Some((start.trim(), Some(end.trim())))
    } else {
        Some((value, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Author, WorkRecord};

    #[test]
    fn csl_retains_core_citation_fields() {
        let mut record = WorkRecord::new("test", "one");
        record.title = "A Study".into();
        record.container_title = "Journal".into();
        record.authors.push(Author {
            family: "Chen".into(),
            given: "Ada".into(),
            ..Author::default()
        });
        record.issued.date_parts = vec![vec![2025, 2, 1]];
        record.ids.insert("doi".into(), "10.1000/test".into());
        let csl = csl_json(&record, "chen2025study");
        assert_eq!(csl["DOI"], "10.1000/test");
        assert_eq!(csl["author"][0]["family"], "Chen");
    }

    #[test]
    fn bibtex_escaping_does_not_corrupt_backslashes() {
        assert_eq!(bibtex_escape(r"A\B_{C}"), r"A\textbackslash{}B\_\{C\}");
    }
}
