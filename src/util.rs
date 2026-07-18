use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn normalize_space(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn normalize_doi(value: &str) -> Option<String> {
    let mut doi = value.trim().to_lowercase();
    for prefix in [
        "https://doi.org/",
        "http://doi.org/",
        "http://dx.doi.org/",
        "doi:",
    ] {
        if let Some(rest) = doi.strip_prefix(prefix) {
            doi = rest.trim().to_owned();
            break;
        }
    }
    doi = doi.trim_end_matches([' ', '.', ';', ',']).to_owned();
    let pattern =
        DOI_PATTERN.get_or_init(|| Regex::new(r"^10\.\d{4,9}/\S+$").expect("static DOI regex"));
    pattern.is_match(&doi).then_some(doi)
}

pub fn normalize_arxiv(value: &str) -> Option<String> {
    let mut id = value.trim().to_owned();
    for prefix in [
        "arXiv:",
        "arxiv:",
        "https://arxiv.org/abs/",
        "https://arxiv.org/pdf/",
        "http://arxiv.org/abs/",
    ] {
        if let Some(rest) = id.strip_prefix(prefix) {
            id = rest.to_owned();
            break;
        }
    }
    if let Some(rest) = id.strip_suffix(".pdf") {
        id = rest.to_owned();
    }
    let pattern = ARXIV_PATTERN.get_or_init(|| {
        Regex::new(r"(?i)^(?:\d{4}\.\d{4,5}|[a-z][a-z0-9.-]*/\d{7})(?:v\d+)?$")
            .expect("static arXiv regex")
    });
    pattern.is_match(&id).then_some(id)
}

pub fn arxiv_base_id(value: &str) -> Option<String> {
    let id = normalize_arxiv(value)?;
    let base = match id.rsplit_once(['v', 'V']) {
        Some((base, version))
            if !base.is_empty()
                && !version.is_empty()
                && version.chars().all(|character| character.is_ascii_digit()) =>
        {
            base
        }
        _ => &id,
    };
    Some(base.to_owned())
}

pub fn arxiv_ids_match(requested: &str, candidate: &str) -> bool {
    let Some(requested) = normalize_arxiv(requested) else {
        return false;
    };
    let Some(candidate) = normalize_arxiv(candidate) else {
        return false;
    };
    let Some(requested_base) = arxiv_base_id(&requested) else {
        return false;
    };
    if requested.len() != requested_base.len() {
        requested.eq_ignore_ascii_case(&candidate)
    } else {
        arxiv_base_id(&candidate)
            .is_some_and(|candidate_base| candidate_base.eq_ignore_ascii_case(&requested_base))
    }
}

pub fn normalize_openalex(value: &str) -> Option<String> {
    let id = value
        .trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let mut characters = id.chars();
    let valid = characters
        .next()
        .is_some_and(|value| value.eq_ignore_ascii_case(&'w'))
        && characters.clone().next().is_some()
        && characters.all(|value| value.is_ascii_digit());
    valid.then(|| format!("W{}", &id[1..]))
}

pub fn title_fingerprint(value: &str) -> String {
    value
        .nfkd()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

pub fn strip_markup(value: &str) -> String {
    let tags = Regex::new(r"<[^>]+>").expect("static regex");
    normalize_space(&tags.replace_all(value, " "))
}

pub fn unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter_map(|value| {
            let cleaned = normalize_space(&value);
            let key = cleaned.to_lowercase();
            (!cleaned.is_empty() && seen.insert(key)).then_some(cleaned)
        })
        .collect()
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("json")
    ));
    let mut output =
        fs::File::create(&temporary).with_context(|| format!("create {}", temporary.display()))?;
    serde_json::to_writer_pretty(&mut output, value)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

pub fn safe_slug(value: &str, fallback: &str) -> String {
    let ascii: String = value
        .nfkd()
        .filter(char::is_ascii)
        .flat_map(char::to_lowercase)
        .collect();
    let separators = Regex::new(r"[^a-z0-9]+").expect("static regex");
    let slug = separators.replace_all(&ascii, "-");
    let slug = slug.trim_matches('-');
    let chosen = if slug.is_empty() { fallback } else { slug };
    chosen.chars().take(96).collect()
}

pub fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exponential = value.exp();
        exponential / (1.0 + exponential)
    }
}

static DOI_PATTERN: OnceLock<Regex> = OnceLock::new();
static ARXIV_PATTERN: OnceLock<Regex> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_persistent_identifiers() {
        assert_eq!(
            normalize_doi("https://doi.org/10.1000/ABC. "),
            Some("10.1000/abc".to_owned())
        );
        assert_eq!(
            normalize_arxiv("https://arxiv.org/pdf/2401.00001.pdf"),
            Some("2401.00001".to_owned())
        );
        assert_eq!(normalize_doi("10.1/not-a-doi"), None);
        assert_eq!(normalize_arxiv("not-an-arxiv-id"), None);
        assert_eq!(
            normalize_openalex("https://openalex.org/w12345"),
            Some("W12345".to_owned())
        );
        assert_eq!(normalize_openalex("not-a-work"), None);
    }

    #[test]
    fn matches_unversioned_arxiv_ids_to_current_versions() {
        assert_eq!(
            arxiv_base_id("https://arxiv.org/abs/2401.00001v2"),
            Some("2401.00001".to_owned())
        );
        assert_eq!(
            arxiv_base_id("hep-th/9901001v3"),
            Some("hep-th/9901001".to_owned())
        );
        assert!(arxiv_ids_match(
            "2401.00001",
            "https://arxiv.org/abs/2401.00001v2"
        ));
        assert!(arxiv_ids_match("hep-th/9901001", "hep-th/9901001v3"));
        assert!(arxiv_ids_match("2401.00001v2", "2401.00001v2"));
        assert!(!arxiv_ids_match("2401.00001v1", "2401.00001v2"));
        assert!(!arxiv_ids_match("2401.00001", "2401.00002v1"));
    }

    #[test]
    fn creates_unicode_stable_title_fingerprint() {
        assert_eq!(title_fingerprint("Café: A Study"), "cafeastudy");
    }
}
