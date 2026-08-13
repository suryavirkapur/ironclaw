use std::sync::Mutex;

use core_agent_memory::{InsertResult, Memori, Memory, SearchQuery, SortField};
use napi::bindgen_prelude::{Error, Result, Status};
use napi_derive::napi;

fn napi_error(error: impl std::fmt::Display) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}

fn parse_json(input: Option<String>) -> Result<Option<serde_json::Value>> {
    input
        .map(|value| serde_json::from_str(&value).map_err(napi_error))
        .transpose()
}

#[napi(object)]
pub struct SearchOptions {
    pub text: Option<String>,
    pub vector: Option<Vec<f64>>,
    pub metadata_filter_json: Option<String>,
    pub limit: Option<u32>,
    pub text_only: Option<bool>,
    pub before: Option<f64>,
    pub after: Option<f64>,
}

#[napi(object)]
pub struct ListOptions {
    pub memory_type: Option<String>,
    pub sort: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub before: Option<f64>,
    pub after: Option<f64>,
}

#[napi(object)]
pub struct StoreResult {
    pub id: String,
    pub deduplicated: bool,
}

#[napi(object)]
pub struct MemoryRecord {
    pub id: String,
    pub content: String,
    pub vector: Option<Vec<f64>>,
    pub metadata_json: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
    pub last_accessed: f64,
    pub access_count: i64,
    pub score: Option<f64>,
}

impl From<Memory> for MemoryRecord {
    fn from(memory: Memory) -> Self {
        Self {
            id: memory.id,
            content: memory.content,
            vector: memory
                .vector
                .map(|values| values.into_iter().map(f64::from).collect()),
            metadata_json: memory.metadata.map(|value| value.to_string()),
            created_at: memory.created_at,
            updated_at: memory.updated_at,
            last_accessed: memory.last_accessed,
            access_count: memory.access_count,
            score: memory.score.map(f64::from),
        }
    }
}

#[napi(object)]
pub struct MemoryStats {
    pub count: u32,
    pub embedded: u32,
    pub type_distribution_json: String,
}

#[napi]
pub struct AgentMemory {
    inner: Mutex<Memori>,
}

#[napi]
impl AgentMemory {
    #[napi(constructor)]
    pub fn new(path: String) -> Result<Self> {
        Ok(Self {
            inner: Mutex::new(Memori::open(&path).map_err(napi_error)?),
        })
    }

    #[napi]
    pub fn store(
        &self,
        content: String,
        metadata_json: Option<String>,
        dedup_threshold: Option<f64>,
        no_embed: Option<bool>,
    ) -> Result<StoreResult> {
        let memory = self.inner.lock().map_err(napi_error)?;
        let result = memory
            .insert(
                &content,
                None,
                parse_json(metadata_json)?,
                dedup_threshold.map(|value| value as f32),
                no_embed.unwrap_or(true),
            )
            .map_err(napi_error)?;
        Ok(StoreResult {
            id: result.id().to_string(),
            deduplicated: matches!(result, InsertResult::Deduplicated(_)),
        })
    }

    #[napi]
    pub fn search(&self, options: SearchOptions) -> Result<Vec<MemoryRecord>> {
        let memory = self.inner.lock().map_err(napi_error)?;
        let results = memory
            .search(SearchQuery {
                vector: options
                    .vector
                    .map(|values| values.into_iter().map(|value| value as f32).collect()),
                text: options.text,
                filter: parse_json(options.metadata_filter_json)?,
                limit: options.limit.unwrap_or(10).clamp(1, 1_000) as usize,
                text_only: options.text_only.unwrap_or(false),
                before: options.before,
                after: options.after,
            })
            .map_err(napi_error)?;
        Ok(results.into_iter().map(MemoryRecord::from).collect())
    }

    #[napi]
    pub fn get(&self, id_or_prefix: String) -> Result<Option<MemoryRecord>> {
        let memory = self.inner.lock().map_err(napi_error)?;
        memory
            .get(&id_or_prefix)
            .map(|item| item.map(MemoryRecord::from))
            .map_err(napi_error)
    }

    #[napi]
    pub fn update(
        &self,
        id_or_prefix: String,
        content: Option<String>,
        metadata_json: Option<String>,
        merge_metadata: Option<bool>,
    ) -> Result<()> {
        let memory = self.inner.lock().map_err(napi_error)?;
        memory
            .update(
                &id_or_prefix,
                content.as_deref(),
                None,
                parse_json(metadata_json)?,
                merge_metadata.unwrap_or(true),
            )
            .map_err(napi_error)
    }

    #[napi]
    pub fn delete(&self, id_or_prefix: String) -> Result<()> {
        self.inner
            .lock()
            .map_err(napi_error)?
            .delete(&id_or_prefix)
            .map_err(napi_error)
    }

    #[napi]
    pub fn list(&self, options: Option<ListOptions>) -> Result<Vec<MemoryRecord>> {
        let options = options.unwrap_or(ListOptions {
            memory_type: None,
            sort: None,
            limit: None,
            offset: None,
            before: None,
            after: None,
        });
        let sort = SortField::from_str(options.sort.as_deref().unwrap_or("created"))
            .map_err(napi_error)?;
        let memory = self.inner.lock().map_err(napi_error)?;
        let records = memory
            .list(
                options.memory_type.as_deref(),
                &sort,
                options.limit.unwrap_or(100).clamp(1, 10_000) as usize,
                options.offset.unwrap_or(0) as usize,
                options.before,
                options.after,
            )
            .map_err(napi_error)?;
        Ok(records.into_iter().map(MemoryRecord::from).collect())
    }

    #[napi]
    pub fn related(&self, id_or_prefix: String, limit: Option<u32>) -> Result<Vec<MemoryRecord>> {
        let memory = self.inner.lock().map_err(napi_error)?;
        let records = memory
            .related(&id_or_prefix, limit.unwrap_or(10).clamp(1, 1_000) as usize)
            .map_err(napi_error)?;
        Ok(records.into_iter().map(MemoryRecord::from).collect())
    }

    #[napi]
    pub fn stats(&self) -> Result<MemoryStats> {
        let memory = self.inner.lock().map_err(napi_error)?;
        let (embedded, total) = memory.embedding_stats().map_err(napi_error)?;
        Ok(MemoryStats {
            count: total.try_into().unwrap_or(u32::MAX),
            embedded: embedded.try_into().unwrap_or(u32::MAX),
            type_distribution_json: serde_json::to_string(
                &memory.type_distribution().map_err(napi_error)?,
            )
            .map_err(napi_error)?,
        })
    }

    #[napi]
    pub fn vacuum(&self) -> Result<()> {
        self.inner
            .lock()
            .map_err(napi_error)?
            .vacuum()
            .map_err(napi_error)
    }
}
