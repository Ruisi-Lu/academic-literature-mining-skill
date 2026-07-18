use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::redirect::{Attempt, Policy};
use reqwest::{Client, StatusCode};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::time::sleep;
use tracing::warn;
use url::Url;

use crate::config::Settings;
use crate::domain::{FullTextCandidate, WorkRecord};
use crate::util::{safe_slug, sha256_bytes};

#[derive(Clone, Debug)]
pub struct DownloadedPdf {
    pub url: String,
    pub path: PathBuf,
    pub sha256: String,
    pub license: String,
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

async fn download_candidate(
    client: &Client,
    settings: &Settings,
    workspace: &Path,
    work_id: &str,
    candidate: &FullTextCandidate,
) -> Result<DownloadedPdf> {
    let url = Url::parse(&candidate.url).context("invalid PDF URL")?;
    safe_remote_url(&url)?;
    let short_hash = &sha256_bytes(work_id.as_bytes())[..12];
    let stem = safe_slug(work_id, "paper");
    let final_path = workspace
        .join("pdfs")
        .join(format!("{stem}-{short_hash}.pdf"));
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

    #[test]
    fn blocks_local_download_targets() {
        assert!(safe_remote_url(&Url::parse("http://127.0.0.1/paper.pdf").unwrap()).is_err());
        assert!(safe_remote_url(&Url::parse("https://arxiv.org/paper.pdf").unwrap()).is_ok());
    }
}
