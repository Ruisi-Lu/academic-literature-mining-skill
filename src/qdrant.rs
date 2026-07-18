use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::warn;

use crate::citations::csl_json;
use crate::config::Settings;
use crate::domain::{PageRecord, PdfArtifact, WorkRecord};
use crate::nvidia::{NvidiaClient, PageModelInput};

#[derive(Clone)]
pub struct QdrantClient {
    client: Client,
    settings: Settings,
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchResult {
    pub page_id: String,
    pub work_id: String,
    pub page_number: u32,
    pub vector_score: f64,
    pub rerank_score: f64,
    pub citation: Value,
    pub pdf_url: String,
    pub pdf_path: String,
    pub pdf_sha256: String,
    pub pdf_license: String,
    pub image_path: String,
}

impl QdrantClient {
    pub fn new(settings: &Settings) -> Result<Self> {
        settings.require_qdrant()?;
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(settings.timeout_seconds))
                .build()?,
            settings: settings.clone(),
        })
    }

    pub async fn ensure_collection(&self) -> Result<()> {
        let url = format!(
            "{}/collections/{}",
            self.settings.qdrant_url, self.settings.qdrant_collection
        );
        let response = self.request(self.client.get(&url)).send().await?;
        if response.status() == StatusCode::NOT_FOUND {
            let payload = json!({
                "vectors": {
                    "content": {
                        "size": self.settings.vector_size,
                        "distance": "Cosine",
                        "on_disk": true
                    }
                },
                "on_disk_payload": true
            });
            self.ok(self
                .request(self.client.put(&url))
                .json(&payload)
                .send()
                .await?)
                .await?;
            self.create_indices().await?;
            return Ok(());
        }
        let response = response.error_for_status()?;
        let value: Value = response.json().await?;
        let size = value
            .pointer("/result/config/params/vectors/content/size")
            .and_then(Value::as_u64)
            .context("unable to read existing Qdrant vector size")? as usize;
        if size != self.settings.vector_size {
            bail!(
                "Qdrant collection vector size is {size}, expected {}; use a new collection or re-ingest",
                self.settings.vector_size
            );
        }
        Ok(())
    }

    pub async fn upsert_pages(
        &self,
        pages: &[(PageRecord, WorkRecord, PdfArtifact)],
        vectors: &[Vec<f32>],
    ) -> Result<()> {
        if pages.len() != vectors.len() {
            bail!("page/vector count mismatch");
        }
        if pages.iter().any(|(_, _, artifact)| {
            artifact.path.is_empty() || artifact.sha256.is_empty() || artifact.url.is_empty()
        }) {
            bail!("one or more indexed pages are missing the preserved PDF artifact metadata");
        }
        let points = pages
            .iter()
            .zip(vectors)
            .map(|((page, record, artifact), vector)| {
                let citation_key = record.identity().replace([':', '/'], "-");
                let embedding_modality = if page.page_text.trim().is_empty() {
                    "image"
                } else {
                    "text_image"
                };
                json!({
                    "id": page.page_id,
                    "vector": {"content": vector},
                    "payload": {
                        "schema_version": record.schema_version,
                        "record_type": "pdf_page",
                        "work_id": page.work_id,
                        "page_number": page.page_number,
                        "image_path": page.image_path,
                        "image_sha256": page.image_sha256,
                        "page_text": page.page_text,
                        "embedding_model": self.settings.embed_model,
                        "embedding_modality": embedding_modality,
                        "citation": csl_json(record, &citation_key),
                        "publication_year": record.year(),
                        "canonical_record": record,
                        "quality": record.quality,
                        "pdf_url": artifact.url,
                        "pdf_path": artifact.path,
                        "pdf_sha256": artifact.sha256,
                        "pdf_license": artifact.license
                    }
                })
            })
            .collect::<Vec<_>>();
        let url = format!(
            "{}/collections/{}/points?wait=true",
            self.settings.qdrant_url, self.settings.qdrant_collection
        );
        self.ok(self
            .request(self.client.put(url))
            .json(&json!({"points": points}))
            .send()
            .await?)
            .await
    }

    pub async fn search(
        &self,
        nvidia: &NvidiaClient,
        query: &str,
        top_k: usize,
        candidate_limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let vector = nvidia.embed_query(query).await?;
        let url = format!(
            "{}/collections/{}/points/query",
            self.settings.qdrant_url, self.settings.qdrant_collection
        );
        let response = self
            .request(self.client.post(url))
            .json(&json!({
                "query": vector,
                "using": "content",
                "limit": candidate_limit.max(top_k),
                "with_payload": true,
                "with_vector": false
            }))
            .send()
            .await?
            .error_for_status()?;
        let value: Value = response.json().await?;
        let points = value
            .pointer("/result/points")
            .or_else(|| value.get("result"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let model_inputs = points
            .iter()
            .map(|point| PageModelInput {
                image_path: PathBuf::from(text(point, "/payload/image_path")),
                text: text(point, "/payload/page_text"),
            })
            .collect::<Vec<_>>();
        let mut scores = Vec::with_capacity(model_inputs.len());
        for chunk in model_inputs.chunks(self.settings.rerank_batch_size) {
            scores.extend(nvidia.rerank_pages(query, chunk).await?);
        }
        let mut results = points
            .into_iter()
            .zip(scores)
            .map(|(point, rerank_score)| SearchResult {
                page_id: point.get("id").map(value_as_string).unwrap_or_default(),
                work_id: text(&point, "/payload/work_id"),
                page_number: point
                    .pointer("/payload/page_number")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                vector_score: point.get("score").and_then(Value::as_f64).unwrap_or(0.0),
                rerank_score,
                citation: point
                    .pointer("/payload/citation")
                    .cloned()
                    .unwrap_or(Value::Null),
                pdf_url: text(&point, "/payload/pdf_url"),
                pdf_path: text(&point, "/payload/pdf_path"),
                pdf_sha256: text(&point, "/payload/pdf_sha256"),
                pdf_license: text(&point, "/payload/pdf_license"),
                image_path: text(&point, "/payload/image_path"),
            })
            .collect::<Vec<_>>();
        results.sort_by(|left, right| rerank_score_order(right, left));
        results.truncate(top_k);
        Ok(results)
    }

    pub async fn health(&self) -> Result<()> {
        let url = format!("{}/readyz", self.settings.qdrant_url);
        self.ok(self.request(self.client.get(url)).send().await?)
            .await
    }

    async fn create_indices(&self) -> Result<()> {
        for (field_name, field_schema) in [
            ("work_id", json!("keyword")),
            ("record_type", json!("keyword")),
            ("citation.DOI", json!("keyword")),
            ("publication_year", json!("integer")),
            ("quality.tier", json!("keyword")),
        ] {
            let url = format!(
                "{}/collections/{}/index?wait=true",
                self.settings.qdrant_url, self.settings.qdrant_collection
            );
            let response = self
                .request(self.client.put(url))
                .json(&json!({
                    "field_name": field_name,
                    "field_schema": field_schema
                }))
                .send()
                .await?;
            if !response.status().is_success() {
                warn!(field_name, status = %response.status(), "payload index was not created");
            }
        }
        Ok(())
    }

    fn request(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.settings.qdrant_api_key.is_empty() {
            builder
        } else {
            builder.header("api-key", &self.settings.qdrant_api_key)
        }
    }

    async fn ok(&self, response: reqwest::Response) -> Result<()> {
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = response.text().await.unwrap_or_default();
            bail!("Qdrant request failed ({status}): {body}")
        }
    }
}

fn text(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .map(value_as_string)
        .unwrap_or_default()
}

fn value_as_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        other => other.to_string(),
    }
}

fn rerank_score_order(left: &SearchResult, right: &SearchResult) -> std::cmp::Ordering {
    left.rerank_score
        .total_cmp(&right.rerank_score)
        .then_with(|| left.vector_score.total_cmp(&right.vector_score))
}
