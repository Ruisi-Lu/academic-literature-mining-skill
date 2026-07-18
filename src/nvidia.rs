use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::sleep;
use tracing::warn;

use crate::config::Settings;
use crate::util::sigmoid;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RerankScore {
    pub logit: f64,
    pub normalized: f64,
}

#[derive(Clone)]
pub struct NvidiaClient {
    client: Client,
    settings: Settings,
}

impl NvidiaClient {
    pub fn new(settings: &Settings) -> Result<Self> {
        settings.require_nvidia()?;
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(settings.timeout_seconds.max(60)))
                .build()?,
            settings: settings.clone(),
        })
    }

    pub async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let payload = json!({
            "model": self.settings.embed_model,
            "input": [query],
            "input_type": "query",
            "modality": "text",
            "embedding_type": "float",
            "encoding_format": "float",
            "truncate": "END"
        });
        let response: EmbeddingResponse = self.post(&self.settings.embed_url, &payload).await?;
        response
            .data
            .into_iter()
            .min_by_key(|item| item.index)
            .map(|item| item.embedding)
            .context("NVIDIA embedding response was empty")
    }

    pub async fn embed_images(&self, paths: &[PathBuf]) -> Result<Vec<Vec<f32>>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = Vec::with_capacity(paths.len());
        for path in paths {
            input.push(image_data_url(path).await?);
        }
        let payload = json!({
            "model": self.settings.embed_model,
            "input": input,
            "input_type": "passage",
            "modality": "image",
            "embedding_type": "float",
            "encoding_format": "float",
            "truncate": "END"
        });
        let response: EmbeddingResponse = self.post(&self.settings.embed_url, &payload).await?;
        let mut data = response.data;
        data.sort_by_key(|item| item.index);
        if data.len() != paths.len() {
            bail!(
                "NVIDIA returned {} embeddings for {} images",
                data.len(),
                paths.len()
            );
        }
        Ok(data.into_iter().map(|item| item.embedding).collect())
    }

    pub async fn rerank_text(&self, query: &str, passages: &[String]) -> Result<Vec<RerankScore>> {
        let values = passages
            .iter()
            .map(|text| json!({"text": text}))
            .collect::<Vec<_>>();
        self.rerank(query, values).await
    }

    pub async fn rerank_images(&self, query: &str, paths: &[PathBuf]) -> Result<Vec<f64>> {
        let mut passages = Vec::with_capacity(paths.len());
        for path in paths {
            passages.push(json!({"image": image_data_url(path).await?}));
        }
        Ok(self
            .rerank(query, passages)
            .await?
            .into_iter()
            .map(|score| score.normalized)
            .collect())
    }

    async fn rerank(&self, query: &str, passages: Vec<Value>) -> Result<Vec<RerankScore>> {
        if passages.is_empty() {
            return Ok(Vec::new());
        }
        let expected = passages.len();
        let payload = json!({
            "model": self.settings.rerank_model,
            "query": {"text": query},
            "passages": passages,
            "truncate": "END"
        });
        let response: RankingResponse = self.post(&self.settings.rerank_url, &payload).await?;
        ordered_rerank_scores(response, expected)
    }

    async fn post<T>(&self, url: &str, payload: &Value) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let mut delay = 1;
        for attempt in 0..5 {
            let response = self
                .client
                .post(url)
                .bearer_auth(&self.settings.nvidia_api_key)
                .header("accept", "application/json")
                .json(payload)
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => {
                    return response
                        .json()
                        .await
                        .context("decode NVIDIA Build response");
                }
                Ok(response)
                    if response.status() == StatusCode::TOO_MANY_REQUESTS
                        || response.status().is_server_error() =>
                {
                    let status = response.status();
                    let retry_after = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(delay);
                    warn!(attempt, %status, retry_after, "retrying NVIDIA request");
                    sleep(Duration::from_secs(retry_after.min(60))).await;
                }
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    bail!("NVIDIA Build request failed ({status}): {body}");
                }
                Err(error) if attempt < 4 => {
                    warn!(attempt, %error, "retrying NVIDIA network failure");
                    sleep(Duration::from_secs(delay)).await;
                }
                Err(error) => return Err(error.into()),
            }
            delay = (delay * 2).min(30);
        }
        bail!("NVIDIA Build request exhausted retries")
    }
}

async fn image_data_url(path: &Path) -> Result<String> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read page image {}", path.display()))?;
    if bytes.len() > 25 * 1024 * 1024 {
        bail!(
            "page image exceeds NVIDIA's 25 MiB limit: {}",
            path.display()
        );
    }
    let mime = match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "image/jpeg",
    };
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct RankingResponse {
    rankings: Vec<RankingItem>,
}

#[derive(Debug, Deserialize)]
struct RankingItem {
    index: usize,
    logit: f64,
}

fn ordered_rerank_scores(response: RankingResponse, expected: usize) -> Result<Vec<RerankScore>> {
    let mut scores = vec![None; expected];
    for ranking in response.rankings {
        if ranking.index < expected {
            scores[ranking.index] = Some(RerankScore {
                logit: ranking.logit,
                normalized: sigmoid(ranking.logit),
            });
        }
    }
    scores
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .context("NVIDIA ranking response omitted one or more passages")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_raw_and_sigmoid_rerank_scores_in_passage_order() {
        let scores = ordered_rerank_scores(
            RankingResponse {
                rankings: vec![
                    RankingItem {
                        index: 1,
                        logit: -3.0,
                    },
                    RankingItem {
                        index: 0,
                        logit: 2.0,
                    },
                ],
            },
            2,
        )
        .unwrap();
        assert_eq!(scores[0].logit, 2.0);
        assert_eq!(scores[0].normalized, sigmoid(2.0));
        assert_eq!(scores[1].logit, -3.0);
        assert_eq!(scores[1].normalized, sigmoid(-3.0));
    }

    #[test]
    fn rejects_incomplete_rerank_responses() {
        let error = ordered_rerank_scores(
            RankingResponse {
                rankings: vec![RankingItem {
                    index: 0,
                    logit: 1.0,
                }],
            },
            2,
        )
        .unwrap_err();
        assert!(error.to_string().contains("omitted one or more passages"));
    }
}
