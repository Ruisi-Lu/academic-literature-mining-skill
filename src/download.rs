use std::fs;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::redirect::{Attempt, Policy};
use reqwest::{Client, StatusCode};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::time::sleep;
use tracing::warn;
use url::Url;

use crate::config::Settings;
use crate::domain::{FullTextCandidate, GOOGLE_SCHOLAR_LIBRARY_ACCESS_FLAG, WorkRecord};
use crate::util::{
    normalize_arxiv, normalize_doi, normalize_openalex, safe_slug, sha256_bytes, sha256_file,
};

pub const USER_SUPPLIED_LICENSE: &str = "user-supplied; reuse rights not established";

#[derive(Clone, Debug)]
pub struct DownloadedPdf {
    pub url: String,
    pub path: PathBuf,
    pub sha256: String,
    pub license: String,
    pub user_supplied: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ManualDownloadRequest {
    pub work_id: String,
    pub title: String,
    pub doi: Option<String>,
    pub download_urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub google_scholar_query_url: Option<String>,
    pub destination: String,
    pub reason: String,
    pub instructions: String,
}

pub fn client(settings: &Settings) -> Result<Client> {
    let policy = Policy::custom(|attempt: Attempt<'_>| {
        if attempt.previous().len() > 10 {
            attempt.error("too many redirects")
        } else if safe_remote_url(attempt.url()).is_err() {
            attempt.error("redirect target is not a public HTTP(S) URL")
        } else {
            attempt.follow()
        }
    });
    Ok(Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds.max(60)))
        .redirect(policy)
        .user_agent(format!(
            "AcademicLiteratureMining/0.1 ({})",
            settings.contact_email
        ))
        .build()?)
}

pub async fn download_work(
    client: &Client,
    settings: &Settings,
    workspace: &Path,
    work_id: &str,
    record: &WorkRecord,
) -> Result<DownloadedPdf> {
    let candidates = record
        .fulltext_candidates
        .iter()
        .filter(|candidate| candidate.authorized)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        bail!("no authorized open-access PDF candidate");
    }
    let mut errors = Vec::new();
    for candidate in candidates {
        match download_candidate(client, settings, workspace, work_id, candidate).await {
            Ok(downloaded) => return Ok(downloaded),
            Err(error) => errors.push(format!("{}: {error:#}", candidate.url)),
        }
    }
    bail!(
        "all authorized PDF candidates failed:\n{}",
        errors.join("\n")
    )
}

pub fn manual_download_request(
    workspace: &Path,
    work_id: &str,
    record: &WorkRecord,
    reason: impl Into<String>,
) -> ManualDownloadRequest {
    ManualDownloadRequest {
        work_id: work_id.to_owned(),
        title: record.title.clone(),
        doi: record.ids.get("doi").and_then(|value| normalize_doi(value)),
        download_urls: manual_download_urls(record),
        google_scholar_query_url: google_scholar_query_url(record),
        destination: pdf_destination(workspace, work_id)
            .to_string_lossy()
            .into_owned(),
        reason: reason.into(),
        instructions: "Use only lawful publisher or institutional access. Download the PDF yourself, save it at destination exactly, do not bypass access controls, and tell the agent when the file is ready. The agent must rerun `litmine download`, then `render`, `ingest`, and `audit`. User-supplied access does not establish a reuse license."
            .to_owned(),
    }
}

pub fn import_manual_pdf(
    workspace: &Path,
    work_id: &str,
    record: &WorkRecord,
    max_pdf_bytes: u64,
) -> Result<DownloadedPdf> {
    let path = pdf_destination(workspace, work_id);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("inspect user-supplied PDF {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("user-supplied PDF must be a regular file, not a symbolic link");
    }
    if !metadata.is_file() {
        bail!("user-supplied PDF path is not a regular file");
    }
    if metadata.len() == 0 {
        bail!("user-supplied PDF is empty");
    }
    if metadata.len() > max_pdf_bytes {
        bail!(
            "user-supplied PDF is {} bytes, exceeding the configured maximum of {max_pdf_bytes}",
            metadata.len()
        );
    }
    let mut input = fs::File::open(&path)?;
    let mut prefix = vec![0_u8; 1024];
    let read = input.read(&mut prefix)?;
    prefix.truncate(read);
    if !prefix.windows(5).any(|window| window == b"%PDF-") {
        bail!("user-supplied file is not a PDF (missing %PDF- signature)");
    }
    let url = manual_download_urls(record)
        .into_iter()
        .next()
        .context("selected work has no publisher, DOI, repository, or full-text URL")?;
    Ok(DownloadedPdf {
        url,
        sha256: sha256_file(&path)?,
        path,
        license: USER_SUPPLIED_LICENSE.to_owned(),
        user_supplied: true,
    })
}

async fn download_candidate(
    client: &Client,
    settings: &Settings,
    workspace: &Path,
    work_id: &str,
    candidate: &FullTextCandidate,
) -> Result<DownloadedPdf> {
    let url = Url::parse(&candidate.url).context("invalid PDF URL")?;
    safe_remote_url(&url)?;
    let final_path = pdf_destination(workspace, work_id);
    let temporary_path = final_path.with_extension("pdf.part");
    let _ = tokio::fs::remove_file(&temporary_path).await;
    let mut delay = 1;
    for attempt in 0..4 {
        let response = client.get(url.clone()).send().await;
        match response {
            Ok(response) if response.status().is_success() => {
                if let Some(length) = response.content_length()
                    && length > settings.max_pdf_bytes
                {
                    bail!("declared PDF size {length} exceeds configured maximum");
                }
                let sha256 =
                    match stream_pdf(response, &temporary_path, settings.max_pdf_bytes).await {
                        Ok(sha256) => sha256,
                        Err(error) if attempt < 3 => {
                            let _ = tokio::fs::remove_file(&temporary_path).await;
                            warn!(attempt, %error, "retrying interrupted PDF response");
                            sleep(Duration::from_secs(delay)).await;
                            delay = (delay * 2).min(20);
                            continue;
                        }
                        Err(error) => {
                            let _ = tokio::fs::remove_file(&temporary_path).await;
                            return Err(error);
                        }
                    };
                tokio::fs::rename(&temporary_path, &final_path).await?;
                return Ok(DownloadedPdf {
                    url: candidate.url.clone(),
                    path: final_path,
                    sha256,
                    license: candidate.license.clone(),
                    user_supplied: false,
                });
            }
            Ok(response)
                if response.status() == StatusCode::TOO_MANY_REQUESTS
                    || response.status().is_server_error() =>
            {
                warn!(attempt, status = %response.status(), "retrying PDF download");
                sleep(Duration::from_secs(delay)).await;
            }
            Ok(response) => bail!("HTTP {}", response.status()),
            Err(error) if attempt < 3 => {
                warn!(attempt, %error, "retrying PDF download failure");
                sleep(Duration::from_secs(delay)).await;
            }
            Err(error) => return Err(error.into()),
        }
        delay = (delay * 2).min(20);
    }
    bail!("PDF download exhausted retries")
}

pub fn pdf_destination(workspace: &Path, work_id: &str) -> PathBuf {
    let short_hash = &sha256_bytes(work_id.as_bytes())[..12];
    let stem = safe_slug(work_id, "paper");
    workspace
        .join("pdfs")
        .join(format!("{stem}-{short_hash}.pdf"))
}

fn manual_download_urls(record: &WorkRecord) -> Vec<String> {
    let mut urls = Vec::new();
    for candidate in &record.fulltext_candidates {
        push_public_url(&mut urls, &candidate.url);
    }
    push_public_url(&mut urls, &record.url);
    if let Some(doi) = record.ids.get("doi").and_then(|value| normalize_doi(value)) {
        push_public_url(&mut urls, &format!("https://doi.org/{doi}"));
    }
    if let Some(openalex) = record
        .ids
        .get("openalex")
        .and_then(|value| normalize_openalex(value))
    {
        push_public_url(&mut urls, &format!("https://openalex.org/{openalex}"));
    }
    if let Some(arxiv) = record
        .ids
        .get("arxiv")
        .and_then(|value| normalize_arxiv(value))
    {
        push_public_url(&mut urls, &format!("https://arxiv.org/abs/{arxiv}"));
    }
    urls
}

fn google_scholar_query_url(record: &WorkRecord) -> Option<String> {
    if !record
        .flags
        .get(GOOGLE_SCHOLAR_LIBRARY_ACCESS_FLAG)
        .copied()
        .unwrap_or(false)
    {
        return None;
    }
    let doi = record
        .ids
        .get("doi")
        .and_then(|value| normalize_doi(value))?;
    let mut url = Url::parse("https://scholar.google.com/scholar").ok()?;
    url.query_pairs_mut().append_pair("q", &doi);
    Some(url.into())
}

fn push_public_url(urls: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() || urls.iter().any(|existing| existing == value) {
        return;
    }
    if let Ok(url) = Url::parse(value)
        && safe_remote_url(&url).is_ok()
    {
        urls.push(value.to_owned());
    }
}

async fn stream_pdf(response: reqwest::Response, path: &Path, max_bytes: u64) -> Result<String> {
    let mut output = tokio::fs::File::create(path).await?;
    let mut stream = response.bytes_stream();
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut prefix = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        total += chunk.len() as u64;
        if total > max_bytes {
            bail!("download exceeded configured maximum PDF size");
        }
        if prefix.len() < 1024 {
            let needed = 1024 - prefix.len();
            prefix.extend_from_slice(&chunk[..chunk.len().min(needed)]);
        }
        digest.update(&chunk);
        output.write_all(&chunk).await?;
    }
    output.flush().await?;
    output.sync_all().await?;
    if !prefix.windows(5).any(|window| window == b"%PDF-") {
        bail!("response is not a PDF (missing %PDF- signature)");
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn safe_remote_url(url: &Url) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only HTTP(S) full-text URLs are allowed");
    }
    let host = url.host_str().context("URL is missing a host")?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".local") {
        bail!("local network targets are not allowed");
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        let private = match ip {
            IpAddr::V4(ip) => {
                ip.is_private()
                    || ip.is_loopback()
                    || ip.is_link_local()
                    || ip.is_broadcast()
                    || ip.is_unspecified()
            }
            IpAddr::V6(ip) => ip.is_loopback() || ip.is_unspecified() || ip.is_unique_local(),
        };
        if private {
            bail!("private network targets are not allowed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn blocks_local_download_targets() {
        assert!(safe_remote_url(&Url::parse("http://127.0.0.1/paper.pdf").unwrap()).is_err());
        assert!(safe_remote_url(&Url::parse("https://arxiv.org/paper.pdf").unwrap()).is_ok());
    }

    #[test]
    fn builds_a_stable_manual_download_handoff() {
        let temporary = tempdir().unwrap();
        let mut record = WorkRecord::new("crossref", "10.1000/manual");
        record.ids.insert("doi".into(), "10.1000/manual".into());
        record.title = "A subscription article".into();
        record.url = "https://publisher.example/article".into();
        record
            .flags
            .insert(GOOGLE_SCHOLAR_LIBRARY_ACCESS_FLAG.into(), true);

        let request = manual_download_request(
            temporary.path(),
            "doi:10.1000/manual",
            &record,
            "manual access required",
        );

        assert_eq!(request.doi.as_deref(), Some("10.1000/manual"));
        assert!(
            request
                .download_urls
                .contains(&"https://publisher.example/article".to_owned())
        );
        assert!(request.destination.ends_with(".pdf"));
        assert!(
            request
                .google_scholar_query_url
                .as_deref()
                .is_some_and(|url| url.contains("10.1000%2Fmanual"))
        );
        assert!(request.instructions.contains("rerun `litmine download`"));
    }

    #[test]
    fn validates_and_hashes_a_user_supplied_pdf() {
        let temporary = tempdir().unwrap();
        fs::create_dir_all(temporary.path().join("pdfs")).unwrap();
        let mut record = WorkRecord::new("crossref", "10.1000/manual");
        record.ids.insert("doi".into(), "10.1000/manual".into());
        let work_id = "doi:10.1000/manual";
        let path = pdf_destination(temporary.path(), work_id);
        fs::write(&path, b"%PDF-1.7\nmanual test").unwrap();

        let artifact = import_manual_pdf(temporary.path(), work_id, &record, 1024 * 1024).unwrap();

        assert!(artifact.user_supplied);
        assert_eq!(artifact.path, path);
        assert_eq!(artifact.license, USER_SUPPLIED_LICENSE);
        assert!(!artifact.sha256.is_empty());
        assert_eq!(artifact.url, "https://doi.org/10.1000/manual");
    }
}
