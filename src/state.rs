use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;

use crate::domain::{PageRecord, PdfArtifact, RawSourceRecord, ResearchPlan, WorkRecord};
use crate::util::{atomic_json, now, sha256_bytes};

pub struct State {
    connection: Connection,
    pub workspace: PathBuf,
}

#[derive(Clone, Debug)]
pub struct StoredArtifactStatus {
    pub work_id: String,
    pub status: String,
    pub pdf_url: String,
    pub pdf_path: String,
    pub pdf_sha256: String,
    pub pdf_license: String,
    pub declared_page_count: u64,
    pub stored_page_count: u64,
    pub indexed_page_count: u64,
    pub last_error: String,
}

#[derive(Clone, Debug)]
pub struct CatalogFilters {
    pub statuses: Vec<String>,
    pub year_from: Option<u32>,
    pub year_to: Option<u32>,
    pub min_quality_score: Option<f64>,
    pub title_contains: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkSummary {
    pub work_id: String,
    pub status: String,
    pub title: String,
    pub publication_year: Option<u32>,
    pub work_type: String,
    pub doi: Option<String>,
    pub venue: String,
    pub quality_score: f64,
    pub relevance_score: f64,
    pub priority_score: f64,
    pub has_pdf: bool,
    pub page_count: u64,
    pub indexed_page_count: u64,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceLocator {
    pub source: String,
    pub source_id: String,
    pub retrieved_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PageLocator {
    pub page_id: String,
    pub page_number: u32,
    pub image_path: String,
    pub image_sha256: String,
    pub width: u32,
    pub height: u32,
    pub indexed_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkDetails {
    pub summary: WorkSummary,
    pub canonical_record: WorkRecord,
    pub pdf_artifact: Option<PdfArtifact>,
    pub provenance_records: Vec<SourceLocator>,
    pub pages: Vec<PageLocator>,
    pub last_error: String,
}

impl State {
    pub fn open(workspace: &Path) -> Result<Self> {
        fs::create_dir_all(workspace).with_context(|| format!("create {}", workspace.display()))?;
        let workspace = fs::canonicalize(workspace)
            .with_context(|| format!("resolve workspace {}", workspace.display()))?;
        for directory in [
            workspace.join("metadata"),
            workspace.join("pdfs"),
            workspace.join("pages"),
            workspace.join("exports"),
            workspace.join("logs"),
        ] {
            fs::create_dir_all(&directory)
                .with_context(|| format!("create {}", directory.display()))?;
        }
        let connection = Connection::open(workspace.join("state.sqlite3"))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS works (
                work_id TEXT PRIMARY KEY,
                canonical_json TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'discovered',
                quality_score REAL NOT NULL DEFAULT 0,
                relevance_score REAL NOT NULL DEFAULT 0,
                priority_score REAL NOT NULL DEFAULT 0,
                pdf_url TEXT,
                pdf_path TEXT,
                pdf_sha256 TEXT,
                pdf_license TEXT,
                page_count INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS works_status_idx
                ON works(status, priority_score DESC);

            CREATE TABLE IF NOT EXISTS source_records (
                source TEXT NOT NULL,
                source_id TEXT NOT NULL,
                work_id TEXT NOT NULL,
                retrieved_at TEXT NOT NULL,
                raw_json TEXT NOT NULL,
                PRIMARY KEY (source, source_id),
                FOREIGN KEY (work_id) REFERENCES works(work_id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS pages (
                page_id TEXT PRIMARY KEY,
                work_id TEXT NOT NULL,
                page_number INTEGER NOT NULL,
                image_path TEXT NOT NULL,
                image_sha256 TEXT NOT NULL,
                page_text TEXT NOT NULL DEFAULT '',
                width INTEGER NOT NULL,
                height INTEGER NOT NULL,
                indexed_at TEXT,
                UNIQUE(work_id, page_number),
                FOREIGN KEY (work_id) REFERENCES works(work_id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS pages_work_idx ON pages(work_id, page_number);

            CREATE TABLE IF NOT EXISTS runs (
                run_id TEXT PRIMARY KEY,
                command TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                details_json TEXT NOT NULL DEFAULT '{}'
            );
            "#,
        )?;
        ensure_page_text_column(&connection)?;
        Ok(Self {
            connection,
            workspace,
        })
    }

    pub fn open_existing(workspace: &Path) -> Result<Self> {
        let workspace = fs::canonicalize(workspace)
            .with_context(|| format!("resolve existing workspace {}", workspace.display()))?;
        let database = workspace.join("state.sqlite3");
        if !database.is_file() {
            anyhow::bail!(
                "live SQLite state does not exist at {}; initialize or mine the corpus first",
                database.display()
            );
        }
        let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self {
            connection,
            workspace,
        })
    }

    pub fn upsert_work(&self, record: &WorkRecord, status: &str) -> Result<String> {
        let work_id = record.identity();
        let json = serde_json::to_string(record)?;
        self.connection.execute(
            r#"
            INSERT INTO works (
                work_id, canonical_json, status, quality_score, relevance_score,
                priority_score, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(work_id) DO UPDATE SET
                canonical_json=excluded.canonical_json,
                status=CASE
                    WHEN works.status IN (
                        'awaiting-manual-download', 'downloaded', 'rendered', 'indexed'
                    )
                    THEN works.status ELSE excluded.status END,
                quality_score=excluded.quality_score,
                relevance_score=excluded.relevance_score,
                priority_score=excluded.priority_score,
                updated_at=excluded.updated_at
            "#,
            params![
                work_id,
                json,
                status,
                record.quality.score,
                record.quality.relevance_score,
                record.quality.priority_score,
                now()
            ],
        )?;
        Ok(work_id)
    }

    pub fn preserve_research_plan(&self, plan: &ResearchPlan) -> Result<String> {
        let serialized = serde_json::to_vec(plan)?;
        let hash = sha256_bytes(&serialized);
        atomic_json(&self.workspace.join("metadata/research-plan.json"), plan)?;
        atomic_json(
            &self
                .workspace
                .join("metadata")
                .join("plans")
                .join(format!("{hash}.json")),
            plan,
        )?;
        Ok(hash)
    }

    pub fn store_raw(&self, work_id: &str, raw: &RawSourceRecord) -> Result<()> {
        self.connection.execute(
            r#"
            INSERT INTO source_records (
                source, source_id, work_id, retrieved_at, raw_json
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(source, source_id) DO UPDATE SET
                work_id=excluded.work_id,
                retrieved_at=excluded.retrieved_at,
                raw_json=excluded.raw_json
            "#,
            params![
                raw.source,
                raw.source_id,
                work_id,
                raw.retrieved_at,
                serde_json::to_string(&raw.raw)?
            ],
        )?;
        Ok(())
    }

    pub fn all_works(&self) -> Result<Vec<(String, WorkRecord, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT work_id, canonical_json, status FROM works ORDER BY priority_score DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (id, json, status) = row?;
            Ok((id, serde_json::from_str(&json)?, status))
        })
        .collect()
    }

    pub fn works_with_statuses(&self, statuses: &[&str]) -> Result<Vec<(String, WorkRecord)>> {
        self.all_works().map(|works| {
            works
                .into_iter()
                .filter(|(_, _, status)| statuses.contains(&status.as_str()))
                .map(|(id, record, _)| (id, record))
                .collect()
        })
    }

    pub fn get_work(&self, work_id: &str) -> Result<Option<WorkRecord>> {
        let json = self
            .connection
            .query_row(
                "SELECT canonical_json FROM works WHERE work_id=?1",
                [work_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        json.map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn catalog(&self, filters: &CatalogFilters) -> Result<Vec<WorkSummary>> {
        if filters.limit == 0 {
            anyhow::bail!("catalog limit must be positive");
        }
        if filters
            .year_from
            .zip(filters.year_to)
            .is_some_and(|(from, to)| from > to)
        {
            anyhow::bail!("year_from must be less than or equal to year_to");
        }
        if filters
            .min_quality_score
            .is_some_and(|score| !(0.0..=100.0).contains(&score))
        {
            anyhow::bail!("min_quality_score must be between 0 and 100");
        }

        let title_filter = filters
            .title_contains
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase);
        let mut statement = self.connection.prepare(
            r#"
            SELECT
                w.work_id,
                w.canonical_json,
                w.status,
                w.quality_score,
                w.relevance_score,
                w.priority_score,
                COALESCE(w.pdf_path, ''),
                w.page_count,
                COALESCE(SUM(CASE WHEN p.indexed_at IS NOT NULL THEN 1 ELSE 0 END), 0),
                w.updated_at
            FROM works w
            LEFT JOIN pages p ON p.work_id=w.work_id
            GROUP BY w.work_id
            ORDER BY w.priority_score DESC, w.work_id
            "#,
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, f64>(4)?,
                row.get::<_, f64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, u64>(7)?,
                row.get::<_, u64>(8)?,
                row.get::<_, String>(9)?,
            ))
        })?;

        let mut summaries = Vec::new();
        for row in rows {
            let (
                work_id,
                canonical_json,
                status,
                quality_score,
                relevance_score,
                priority_score,
                pdf_path,
                page_count,
                indexed_page_count,
                updated_at,
            ) = row?;
            if !filters.statuses.is_empty()
                && !filters
                    .statuses
                    .iter()
                    .any(|candidate| candidate == &status)
            {
                continue;
            }
            let record: WorkRecord = serde_json::from_str(&canonical_json)?;
            let year = record.year();
            if filters
                .year_from
                .is_some_and(|minimum| year.is_none_or(|value| value < minimum))
                || filters
                    .year_to
                    .is_some_and(|maximum| year.is_none_or(|value| value > maximum))
                || filters
                    .min_quality_score
                    .is_some_and(|minimum| quality_score < minimum)
                || title_filter
                    .as_ref()
                    .is_some_and(|needle| !record.title.to_lowercase().contains(needle))
            {
                continue;
            }
            summaries.push(work_summary(
                work_id,
                status,
                &record,
                quality_score,
                relevance_score,
                priority_score,
                !pdf_path.is_empty(),
                page_count,
                indexed_page_count,
                updated_at,
            ));
            if summaries.len() == filters.limit {
                break;
            }
        }
        Ok(summaries)
    }

    pub fn inspect_work(&self, work_id: &str) -> Result<Option<WorkDetails>> {
        type WorkRow = (
            String,
            String,
            f64,
            f64,
            f64,
            String,
            String,
            String,
            String,
            u64,
            u64,
            String,
            String,
        );

        let row: Option<WorkRow> = self
            .connection
            .query_row(
                r#"
                SELECT
                    w.canonical_json,
                    w.status,
                    w.quality_score,
                    w.relevance_score,
                    w.priority_score,
                    COALESCE(w.pdf_url, ''),
                    COALESCE(w.pdf_path, ''),
                    COALESCE(w.pdf_sha256, ''),
                    COALESCE(w.pdf_license, ''),
                    w.page_count,
                    COALESCE(SUM(CASE WHEN p.indexed_at IS NOT NULL THEN 1 ELSE 0 END), 0),
                    COALESCE(w.last_error, ''),
                    w.updated_at
                FROM works w
                LEFT JOIN pages p ON p.work_id=w.work_id
                WHERE w.work_id=?1
                GROUP BY w.work_id
                "#,
                [work_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            canonical_json,
            status,
            quality_score,
            relevance_score,
            priority_score,
            pdf_url,
            pdf_path,
            pdf_sha256,
            pdf_license,
            page_count,
            indexed_page_count,
            last_error,
            updated_at,
        )) = row
        else {
            return Ok(None);
        };
        let canonical_record: WorkRecord = serde_json::from_str(&canonical_json)?;
        let summary = work_summary(
            work_id.to_owned(),
            status,
            &canonical_record,
            quality_score,
            relevance_score,
            priority_score,
            !pdf_path.is_empty(),
            page_count,
            indexed_page_count,
            updated_at,
        );

        let mut source_statement = self.connection.prepare(
            r#"
            SELECT source, source_id, retrieved_at
            FROM source_records
            WHERE work_id=?1
            ORDER BY source, source_id
            "#,
        )?;
        let provenance_records = source_statement
            .query_map([work_id], |row| {
                Ok(SourceLocator {
                    source: row.get(0)?,
                    source_id: row.get(1)?,
                    retrieved_at: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut page_statement = self.connection.prepare(
            r#"
            SELECT page_id, page_number, image_path, image_sha256,
                   width, height, indexed_at
            FROM pages
            WHERE work_id=?1
            ORDER BY page_number
            "#,
        )?;
        let pages = page_statement
            .query_map([work_id], |row| {
                Ok(PageLocator {
                    page_id: row.get(0)?,
                    page_number: row.get(1)?,
                    image_path: row.get(2)?,
                    image_sha256: row.get(3)?,
                    width: row.get(4)?,
                    height: row.get(5)?,
                    indexed_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let pdf_artifact = (!pdf_path.is_empty()).then_some(PdfArtifact {
            url: pdf_url,
            path: pdf_path,
            sha256: pdf_sha256,
            license: pdf_license,
        });
        Ok(Some(WorkDetails {
            summary,
            canonical_record,
            pdf_artifact,
            provenance_records,
            pages,
            last_error,
        }))
    }

    pub fn mark_downloaded(
        &self,
        work_id: &str,
        url: &str,
        path: &Path,
        sha256: &str,
        license: &str,
    ) -> Result<()> {
        self.connection.execute(
            r#"
            UPDATE works SET
                status='downloaded', pdf_url=?2, pdf_path=?3, pdf_sha256=?4,
                pdf_license=?5, last_error=NULL, updated_at=?6
            WHERE work_id=?1
            "#,
            params![work_id, url, path.to_string_lossy(), sha256, license, now()],
        )?;
        Ok(())
    }

    pub fn mark_awaiting_manual_download(&self, work_id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE works SET status='awaiting-manual-download', last_error=NULL, updated_at=?2 WHERE work_id=?1",
            params![work_id, now()],
        )?;
        Ok(())
    }

    pub fn mark_error(&self, work_id: &str, stage: &str, error: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE works SET status=?2, last_error=?3, updated_at=?4 WHERE work_id=?1",
            params![work_id, format!("error:{stage}"), error, now()],
        )?;
        Ok(())
    }

    pub fn pdf_for_work(&self, work_id: &str) -> Result<Option<PathBuf>> {
        let value = self
            .connection
            .query_row(
                "SELECT pdf_path FROM works WHERE work_id=?1",
                [work_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        Ok(value.map(PathBuf::from))
    }

    pub fn replace_pages(&mut self, work_id: &str, pages: &[PageRecord]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM pages WHERE work_id=?1", [work_id])?;
        {
            let mut statement = transaction.prepare(
                r#"
                INSERT INTO pages (
                    page_id, work_id, page_number, image_path, image_sha256,
                    page_text, width, height, indexed_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                "#,
            )?;
            for page in pages {
                statement.execute(params![
                    page.page_id,
                    page.work_id,
                    page.page_number,
                    page.image_path,
                    page.image_sha256,
                    page.page_text,
                    page.width,
                    page.height,
                    page.indexed_at
                ])?;
            }
        }
        transaction.execute(
            "UPDATE works SET status='rendered', page_count=?2, last_error=NULL, updated_at=?3 WHERE work_id=?1",
            params![work_id, pages.len(), now()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn pages_for_indexing(&self) -> Result<Vec<(PageRecord, WorkRecord, PdfArtifact)>> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT p.page_id, p.work_id, p.page_number, p.image_path, p.image_sha256,
                   p.page_text, p.width, p.height, p.indexed_at, w.canonical_json,
                   COALESCE(w.pdf_url, ''), COALESCE(w.pdf_path, ''),
                   COALESCE(w.pdf_sha256, ''), COALESCE(w.pdf_license, '')
            FROM pages p JOIN works w ON p.work_id=w.work_id
            WHERE p.indexed_at IS NULL
            ORDER BY p.work_id, p.page_number
            "#,
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                PageRecord {
                    page_id: row.get(0)?,
                    work_id: row.get(1)?,
                    page_number: row.get(2)?,
                    image_path: row.get(3)?,
                    image_sha256: row.get(4)?,
                    page_text: row.get(5)?,
                    width: row.get(6)?,
                    height: row.get(7)?,
                    indexed_at: row.get(8)?,
                },
                row.get::<_, String>(9)?,
                PdfArtifact {
                    url: row.get(10)?,
                    path: row.get(11)?,
                    sha256: row.get(12)?,
                    license: row.get(13)?,
                },
            ))
        })?;
        rows.map(|row| {
            let (page, json, artifact) = row?;
            Ok((page, serde_json::from_str(&json)?, artifact))
        })
        .collect()
    }

    pub fn mark_pages_indexed(&mut self, pages: &[PageRecord]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        let timestamp = now();
        for page in pages {
            transaction.execute(
                "UPDATE pages SET indexed_at=?2 WHERE page_id=?1",
                params![page.page_id, timestamp],
            )?;
        }
        transaction.execute(
            r#"
            UPDATE works SET status='indexed', updated_at=?1
            WHERE work_id IN (
                SELECT DISTINCT work_id FROM pages
                WHERE indexed_at IS NOT NULL
            )
            AND NOT EXISTS (
                SELECT 1 FROM pages pending
                WHERE pending.work_id=works.work_id AND pending.indexed_at IS NULL
            )
            "#,
            [&timestamp],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn summary(&self) -> Result<Vec<(String, u64)>> {
        let mut statement = self
            .connection
            .prepare("SELECT status, COUNT(*) FROM works GROUP BY status")?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn artifact_statuses(&self) -> Result<Vec<StoredArtifactStatus>> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT
                w.work_id,
                w.status,
                COALESCE(w.pdf_url, ''),
                COALESCE(w.pdf_path, ''),
                COALESCE(w.pdf_sha256, ''),
                COALESCE(w.pdf_license, ''),
                w.page_count,
                COUNT(p.page_id),
                COALESCE(SUM(CASE WHEN p.indexed_at IS NOT NULL THEN 1 ELSE 0 END), 0),
                COALESCE(w.last_error, '')
            FROM works w
            LEFT JOIN pages p ON p.work_id=w.work_id
            GROUP BY w.work_id
            ORDER BY w.work_id
            "#,
        )?;
        statement
            .query_map([], |row| {
                Ok(StoredArtifactStatus {
                    work_id: row.get(0)?,
                    status: row.get(1)?,
                    pdf_url: row.get(2)?,
                    pdf_path: row.get(3)?,
                    pdf_sha256: row.get(4)?,
                    pdf_license: row.get(5)?,
                    declared_page_count: row.get(6)?,
                    stored_page_count: row.get(7)?,
                    indexed_page_count: row.get(8)?,
                    last_error: row.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

#[allow(clippy::too_many_arguments)]
fn work_summary(
    work_id: String,
    status: String,
    record: &WorkRecord,
    quality_score: f64,
    relevance_score: f64,
    priority_score: f64,
    has_pdf: bool,
    page_count: u64,
    indexed_page_count: u64,
    updated_at: String,
) -> WorkSummary {
    WorkSummary {
        work_id,
        status,
        title: record.title.clone(),
        publication_year: record.year(),
        work_type: record.work_type.clone(),
        doi: record.ids.get("doi").cloned(),
        venue: record.container_title.clone(),
        quality_score,
        relevance_score,
        priority_score,
        has_pdf,
        page_count,
        indexed_page_count,
        updated_at,
    }
}

fn ensure_page_text_column(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(pages)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "page_text") {
        connection.execute(
            "ALTER TABLE pages ADD COLUMN page_text TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn carries_original_pdf_provenance_into_indexing_records() {
        let temporary = tempdir().unwrap();
        let mut state = State::open(&temporary.path().join("corpus")).unwrap();
        let mut record = WorkRecord::new("test", "10.1000/test");
        record.ids.insert("doi".into(), "10.1000/test".into());
        record.title = "A paper".into();
        let work_id = state.upsert_work(&record, "selected").unwrap();
        let pdf_path = state.workspace.join("pdfs").join("paper.pdf");
        fs::write(&pdf_path, b"%PDF-1.7").unwrap();
        state
            .mark_downloaded(
                &work_id,
                "https://example.org/paper.pdf",
                &pdf_path,
                "pdf-sha256",
                "https://creativecommons.org/licenses/by/4.0/",
            )
            .unwrap();
        let image_path = state.workspace.join("pages").join("page.jpg");
        fs::write(&image_path, b"image").unwrap();
        state
            .replace_pages(
                &work_id,
                &[PageRecord {
                    page_id: "page-id".into(),
                    work_id: work_id.clone(),
                    page_number: 1,
                    image_path: image_path.to_string_lossy().into_owned(),
                    image_sha256: "image-sha256".into(),
                    page_text: "Native PDF text".into(),
                    width: 100,
                    height: 200,
                    indexed_at: None,
                }],
            )
            .unwrap();
        let pages = state.pages_for_indexing().unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].0.page_text, "Native PDF text");
        assert_eq!(pages[0].2.sha256, "pdf-sha256");
        assert_eq!(pages[0].2.path, pdf_path.to_string_lossy());
        assert!(state.workspace.is_absolute());
    }

    #[test]
    fn migrates_existing_page_tables_for_native_text() {
        let temporary = tempdir().unwrap();
        let workspace = temporary.path().join("corpus");
        fs::create_dir_all(&workspace).unwrap();
        let connection = Connection::open(workspace.join("state.sqlite3")).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE pages (
                    page_id TEXT PRIMARY KEY,
                    work_id TEXT NOT NULL,
                    page_number INTEGER NOT NULL,
                    image_path TEXT NOT NULL,
                    image_sha256 TEXT NOT NULL,
                    width INTEGER NOT NULL,
                    height INTEGER NOT NULL,
                    indexed_at TEXT
                );
                "#,
            )
            .unwrap();
        drop(connection);

        let state = State::open(&workspace).unwrap();
        let columns = state
            .connection
            .prepare("PRAGMA table_info(pages)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "page_text"));
    }

    #[test]
    fn catalog_and_inspect_query_sqlite_without_reading_export_archives() {
        let temporary = tempdir().unwrap();
        let workspace = temporary.path().join("corpus");
        let mut state = State::open(&workspace).unwrap();
        let mut record = WorkRecord::new("crossref", "10.1000/live");
        record.ids.insert("doi".into(), "10.1000/live".into());
        record.title = "Live database record".into();
        record.work_type = "article-journal".into();
        record.container_title = "Journal of Tests".into();
        record.issued.date_parts = vec![vec![2025]];
        record.quality.score = 88.0;
        record.quality.relevance_score = 0.8;
        record.quality.priority_score = 0.9;
        let work_id = state.upsert_work(&record, "selected").unwrap();
        state
            .store_raw(
                &work_id,
                &RawSourceRecord {
                    source: "crossref".into(),
                    source_id: "10.1000/live".into(),
                    retrieved_at: "2026-07-20T00:00:00Z".into(),
                    raw: serde_json::json!({"title": "raw provider record"}),
                },
            )
            .unwrap();
        let page = PageRecord {
            page_id: "page-live".into(),
            work_id: work_id.clone(),
            page_number: 3,
            image_path: workspace.join("pages/page-live.jpg").display().to_string(),
            image_sha256: "page-sha".into(),
            page_text: "Text that must not leak through metadata inspection".into(),
            width: 100,
            height: 200,
            indexed_at: None,
        };
        state
            .replace_pages(&work_id, std::slice::from_ref(&page))
            .unwrap();
        state.mark_pages_indexed(&[page]).unwrap();

        fs::write(
            workspace.join("exports/records.jsonl"),
            r#"{"title":"Stale archive-only record"}"#,
        )
        .unwrap();

        let catalog = state
            .catalog(&CatalogFilters {
                statuses: vec!["indexed".into()],
                year_from: Some(2025),
                year_to: Some(2025),
                min_quality_score: Some(80.0),
                title_contains: Some("live database".into()),
                limit: 10,
            })
            .unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].title, "Live database record");

        let details = state.inspect_work(&work_id).unwrap().unwrap();
        assert_eq!(details.provenance_records.len(), 1);
        assert_eq!(details.pages[0].page_number, 3);
        let serialized = serde_json::to_value(details).unwrap();
        assert!(serialized.pointer("/pages/0/page_text").is_none());
        assert!(serialized.get("raw_json").is_none());
        assert!(!serialized.to_string().contains("Stale archive-only record"));
        assert!(
            !serialized
                .to_string()
                .contains("Text that must not leak through metadata inspection")
        );
    }

    #[test]
    fn opening_live_query_state_never_creates_a_missing_workspace() {
        let temporary = tempdir().unwrap();
        let missing = temporary.path().join("missing-corpus");
        assert!(State::open_existing(&missing).is_err());
        assert!(!missing.exists());
    }
}
