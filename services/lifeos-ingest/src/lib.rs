//! lifeos-ingest: the media→text orchestrator (issue #88, docs/MEDIA-INTELLIGENCE.md).
//!
//! `lifeos-drain` claims `ingest` jobs from `jobs` and calls `process_ingest_job`
//! directly as a library (no subprocess - both crates are Rust in the same
//! workspace). This crate has no dependency on `lifeos-api` (same standalone
//! style as `lifeos-drain`): it reads/writes `entities`/`events` with its own
//! small SQL, mirroring `lifeos-drain`'s `emit_event` mirror of
//! `lifeos_api::audit::emit`.
//!
//! Scope of #88 is the orchestrator, not the extractors: MIME routing, job
//! dispatch, segment-entity creation, and triggering memvec indexing. #89
//! added real transcription for `.mp3/.wav/.m4a` via `symphonia` (decode) +
//! `whisper-rs` (transcribe) - see `audio::decode_to_16k_mono_f32` and
//! `WhisperTranscriber`. #90 added real image captioning (`vision::
//! HaikuCaptioner`, Anthropic vision API) + OCR (`ocr::TesseractOcr`,
//! `tesseract` CLI subprocess) for images, and text extraction for PDF
//! (`docs_extract::extract_pdf_pages`, `pdf-extract`) and docx
//! (`docs_extract::extract_docx_text`, zip+XML). Video containers
//! (`.mov/.webm/.mp4`) remain an honest `Unsupported { kind, blocked_by }`
//! gap, matching the same `blocked_by` issue numbers `lifeos_vcs::diff::
//! blocking_issue_for` already uses for that MIME class.

use async_trait::async_trait;
use libsql::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fmt;

pub mod audio;
pub use audio::AudioError;
pub mod docs_extract;
pub mod ocr;
pub mod vision;
pub use ocr::{NoopOcr, Ocr, TesseractOcr};
pub use vision::{Captioner, HaikuCaptioner, NoopCaptioner};
use std::sync::Mutex;
use ulid::{Generator, Ulid};

static ID_GENERATOR: Mutex<Generator> = Mutex::new(Generator::new());

fn new_id(prefix: &str) -> String {
    let ulid = ID_GENERATOR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .generate()
        .unwrap_or_else(|_| Ulid::new());
    format!("{prefix}_{ulid}")
}

/// Mirrors `lifeos_api::audit::emit` / `lifeos_drain::emit_event`'s shape
/// exactly (same table, same id scheme) so events this crate writes are
/// indistinguishable from ones the API writes.
async fn emit_event(
    conn: &Connection,
    workspace_id: &str,
    event_type: &str,
    entity_id: &str,
    attrs: &Value,
    now: i64,
) -> libsql::Result<()> {
    let attrs_str = serde_json::to_string(attrs).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO events (id, workspace_id, ts, type, entity_id, actor, attrs) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'lifeos-ingest', ?6)",
        params![new_id("evt"), workspace_id, now, event_type, entity_id, attrs_str],
    )
    .await?;
    Ok(())
}

/// A claimed `ingest` job's payload (`routes/planned.rs::IngestRequest`'s
/// enqueued shape).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IngestJobPayload {
    pub entity_id: Option<String>,
    pub uri: Option<String>,
    pub kind: Option<String>,
    pub blob_ref: Option<String>,
}

/// MIME routing decision. `PlainText`/`Audio` have real extractors; every
/// other kind is a named, honest gap - never a silent no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MimeRoute {
    PlainText,
    /// `.mp3/.wav/.m4a` - symphonia can demux+decode all three (issue #89).
    Audio,
    /// `.png/.jpg/.jpeg/.gif/.webp` - vision-LLM caption + tesseract OCR (issue #90).
    Image,
    /// `.pdf` - `pdf-extract` per-page text (issue #90).
    Pdf,
    /// `.docx` - zip+XML text pull (issue #90).
    Docx,
    Unsupported { kind: &'static str, blocked_by: &'static str },
}

/// Routes a file name (falling back to a job's `kind` hint) to its MIME
/// class. Same issue numbers `lifeos_vcs::diff::blocking_issue_for` already
/// names for these MIME classes, so both subsystems agree on what's blocked.
pub fn route_by_mime(name: &str, hint_kind: Option<&str>) -> MimeRoute {
    let lower = name.to_ascii_lowercase();
    const TEXT_EXT: &[&str] = &[".txt", ".md", ".markdown", ".csv", ".log", ".json"];
    const AUDIO_EXT: &[&str] = &[".mp3", ".wav", ".m4a"];
    const VIDEO_EXT: &[&str] = &[".mp4", ".mov", ".webm"];
    const IMAGE_EXT: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".webp"];
    // symphonia has no general video-container demuxer here - out of #89's
    // scope (audio transcription only). Not #89 anymore since #89 is closed
    // by the time this ships; name the real remaining gap instead.
    const VIDEO_GAP: &str = "lifeos-ingest video container support (no symphonia demuxer for mov/webm/mp4-video yet)";

    if TEXT_EXT.iter().any(|ext| lower.ends_with(ext)) {
        return MimeRoute::PlainText;
    }
    if AUDIO_EXT.iter().any(|ext| lower.ends_with(ext)) {
        return MimeRoute::Audio;
    }
    if VIDEO_EXT.iter().any(|ext| lower.ends_with(ext)) {
        return MimeRoute::Unsupported { kind: "video", blocked_by: VIDEO_GAP };
    }
    if IMAGE_EXT.iter().any(|ext| lower.ends_with(ext)) {
        return MimeRoute::Image;
    }
    if lower.ends_with(".pdf") {
        return MimeRoute::Pdf;
    }
    if lower.ends_with(".docx") {
        return MimeRoute::Docx;
    }
    match hint_kind {
        Some("audio") => MimeRoute::Audio,
        Some("video") => MimeRoute::Unsupported { kind: "video", blocked_by: VIDEO_GAP },
        Some("image") => MimeRoute::Image,
        _ => MimeRoute::Unsupported { kind: "unknown", blocked_by: "no MIME route defined for this file type yet" },
    }
}

/// Maps an image file name to the media type the Anthropic vision API
/// expects. Defaults to PNG for unrecognized/hint-only routes - the API
/// rejects a wrong media type outright, so this is a best-effort guess, not
/// a validated content-sniff.
fn image_media_type(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
    }
}

/// Splits plain text into blank-line-separated paragraphs. Honest, no
/// fabricated NLP segmentation - a chunk is exactly what it looks like.
pub fn chunk_plain_text(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

/// Fire-and-forget trigger for memvec indexing. A failed embed never blocks
/// the DB state transition - the boot `rebuild` reconciles any drift, same
/// reasoning as `lifeos_api::db::index_entity`'s doc comment.
#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, workspace_id: &str, entity_id: &str, text: &str);
}

/// Used when no memvec is configured (`LIFEOS_MEMVEC` unset) - segments are
/// still created and lexically indexable, just not semantically searchable.
pub struct NoopEmbedder;

#[async_trait]
impl Embedder for NoopEmbedder {
    async fn embed(&self, _workspace_id: &str, entity_id: &str, _text: &str) {
        eprintln!("lifeos-ingest: no embedder configured, skipping semantic index for {entity_id}");
    }
}

/// Shells out to `server/memvec.py embed`, the same subprocess shape
/// `routes/search.rs`'s query call already uses.
pub struct SubprocessEmbedder {
    pub memvec_path: String,
    pub derived_db_path: String,
}

#[async_trait]
impl Embedder for SubprocessEmbedder {
    async fn embed(&self, workspace_id: &str, entity_id: &str, text: &str) {
        let result = tokio::process::Command::new("python3")
            .arg(&self.memvec_path)
            .arg("--db")
            .arg(&self.derived_db_path)
            .arg("embed")
            .arg("--workspace")
            .arg(workspace_id)
            .arg("--id")
            .arg(entity_id)
            .arg("--text")
            .arg(text)
            .output()
            .await;
        match result {
            Ok(out) if out.status.success() => {}
            Ok(out) => eprintln!(
                "lifeos-ingest: memvec embed failed for {entity_id}: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
            Err(e) => eprintln!("lifeos-ingest: memvec embed spawn failed for {entity_id}: {e}"),
        }
    }
}

/// One transcribed span, real timestamps (issue #89) - the `t_start`/`t_end`
/// fields docs/MEDIA-INTELLIGENCE.md §5 always specified but #88's plain-text
/// path had no locator for.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptSegment {
    pub text: String,
    pub t_start_secs: f64,
    pub t_end_secs: f64,
}

/// Runs 16kHz mono f32 PCM through a real transcription model. DI trait,
/// same shape as `Embedder`/`lifeos_drain::Notifier`/`ModuleBuilder`.
#[async_trait]
pub trait Transcriber: Send + Sync {
    async fn transcribe(&self, samples: &[f32]) -> Result<Vec<TranscriptSegment>, String>;
}

/// Used when `LIFEOS_WHISPER_MODEL` is unset. Unlike `NoopEmbedder` (which
/// silently degrades - a missing memvec still leaves lexically-searchable
/// segments), a missing transcriber for an audio file `route_by_mime` says
/// it CAN handle is a real capability gap: fail loudly instead of silently
/// producing zero segments for content the router promised to process.
pub struct NoopTranscriber;

#[async_trait]
impl Transcriber for NoopTranscriber {
    async fn transcribe(&self, _samples: &[f32]) -> Result<Vec<TranscriptSegment>, String> {
        Err("no whisper model configured (LIFEOS_WHISPER_MODEL unset)".to_string())
    }
}

/// Real whisper.cpp transcription via `whisper-rs`. Inference is CPU-bound
/// sync work, so it runs on `spawn_blocking` rather than the async
/// `lifeos-drain` poll loop's thread.
pub struct WhisperTranscriber {
    pub model_path: String,
}

#[async_trait]
impl Transcriber for WhisperTranscriber {
    async fn transcribe(&self, samples: &[f32]) -> Result<Vec<TranscriptSegment>, String> {
        let model_path = self.model_path.clone();
        let samples = samples.to_vec();
        tokio::task::spawn_blocking(move || run_whisper(&model_path, &samples))
            .await
            .map_err(|e| format!("transcription task panicked: {e}"))?
    }
}

fn run_whisper(model_path: &str, samples: &[f32]) -> Result<Vec<TranscriptSegment>, String> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
        .map_err(|e| format!("failed to load whisper model at '{model_path}': {e}"))?;
    let mut state = ctx.create_state().map_err(|e| format!("failed to create whisper state: {e}"))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_translate(false);
    params.set_no_timestamps(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state.full(params, samples).map_err(|e| format!("whisper inference failed: {e}"))?;

    let n = state.full_n_segments();
    let mut segments = Vec::with_capacity(n as usize);
    for i in 0..n {
        let Some(seg) = state.get_segment(i) else { continue };
        let text = seg.to_str().map_err(|e| format!("failed to read segment {i} text: {e}"))?;
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        segments.push(TranscriptSegment {
            text: trimmed,
            t_start_secs: seg.start_timestamp() as f64 / 100.0,
            t_end_secs: seg.end_timestamp() as f64 / 100.0,
        });
    }
    Ok(segments)
}

/// Decodes and transcribes raw audio `bytes` into one flat transcript string,
/// reusing the same `audio::decode_audio` (symphonia + optional ffmpeg
/// fallback) and `Transcriber` seam the entity-centric ingest path uses. This
/// is the primitive `lifeos-drain`'s voice-note turn (issue #143) calls: it
/// needs a transcript from bytes, not a segment-entity tree. `ffmpeg_bin`
/// enables the OGG/Opus fallback (Telegram voice notes); see `audio` doc.
pub async fn transcribe_audio_bytes(
    bytes: &[u8],
    transcriber: &dyn Transcriber,
    ffmpeg_bin: Option<&str>,
) -> Result<String, String> {
    let samples = audio::decode_audio(bytes, ffmpeg_bin).await.map_err(|e| e.to_string())?;
    let segments = transcriber.transcribe(&samples).await?;
    let text = segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(text.trim().to_string())
}

/// A completed (or honestly-degraded) ingest run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    Completed { segment_ids: Vec<String> },
    Unsupported { kind: String, blocked_by: String },
}

/// A real failure - bad input, not a MIME gap. `lifeos-drain` fails the job
/// (no retry-forever loop) rather than completing it.
#[derive(Debug)]
pub enum IngestError {
    MissingEntityId,
    EntityNotFound(String),
    NoContent,
    Db(libsql::Error),
    Io(std::io::Error),
    Audio(AudioError),
    Transcription(String),
    Caption(String),
    Extraction(String),
}

impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IngestError::MissingEntityId => write!(f, "ingest job payload has no entity_id"),
            IngestError::EntityNotFound(id) => write!(f, "entity '{id}' not found in this workspace"),
            IngestError::NoContent => write!(f, "no blob_ref available (neither payload nor entity has one)"),
            IngestError::Db(e) => write!(f, "db error: {e}"),
            IngestError::Io(e) => write!(f, "blob store error: {e}"),
            IngestError::Audio(e) => write!(f, "{e}"),
            IngestError::Transcription(e) => write!(f, "transcription failed: {e}"),
            IngestError::Caption(e) => write!(f, "captioning failed: {e}"),
            IngestError::Extraction(e) => write!(f, "text extraction failed: {e}"),
        }
    }
}

struct SourceEntity {
    module: String,
    title: Option<String>,
    attrs: Value,
    blob_ref: Option<String>,
}

async fn fetch_entity(conn: &Connection, workspace_id: &str, id: &str) -> Result<SourceEntity, IngestError> {
    let mut rows = conn
        .query(
            "SELECT module, title, attrs, blob_ref FROM entities WHERE id=?1 AND workspace_id=?2",
            params![id, workspace_id],
        )
        .await
        .map_err(IngestError::Db)?;
    match rows.next().await.map_err(IngestError::Db)? {
        Some(row) => {
            let module: String = row.get(0).map_err(IngestError::Db)?;
            let title: Option<String> = row.get(1).map_err(IngestError::Db)?;
            let attrs_str: String = row.get(2).map_err(IngestError::Db)?;
            let blob_ref: Option<String> = row.get(3).map_err(IngestError::Db)?;
            let attrs: Value = serde_json::from_str(&attrs_str).unwrap_or_else(|_| json!({}));
            Ok(SourceEntity { module, title, attrs, blob_ref })
        }
        None => Err(IngestError::EntityNotFound(id.to_string())),
    }
}

fn entity_display_name(entity: &SourceEntity, payload: &IngestJobPayload) -> String {
    if let Some(name) = entity.attrs.get("name").and_then(|v| v.as_str()) {
        return name.to_string();
    }
    if let Some(title) = &entity.title {
        return title.clone();
    }
    if let Some(uri) = &payload.uri {
        return uri.rsplit('/').next().unwrap_or(uri).to_string();
    }
    String::new()
}

/// Inserts one `type=segment` child entity, calls `embedder.embed`, and
/// returns its id. Shared by the plain-text (#88) and audio (#89) paths -
/// `t_start`/`t_end` are `None` for plain text (no real locator) and
/// `Some` for transcribed audio.
#[allow(clippy::too_many_arguments)]
async fn insert_segment(
    conn: &Connection,
    embedder: &(dyn Embedder + Sync),
    workspace_id: &str,
    module: &str,
    parent_id: &str,
    text: &str,
    t_start_secs: Option<f64>,
    t_end_secs: Option<f64>,
    page: Option<u32>,
    now: i64,
) -> Result<String, IngestError> {
    let seg_id = new_id("segment");
    let mut attrs = json!({ "text": text });
    if let (Some(t0), Some(t1)) = (t_start_secs, t_end_secs) {
        attrs["t_start"] = json!(t0);
        attrs["t_end"] = json!(t1);
    }
    if let Some(p) = page {
        attrs["page"] = json!(p);
    }
    conn.execute(
        "INSERT INTO entities (id, workspace_id, module, type, parent_id, attrs, source, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'segment', ?4, ?5, 'lifeos-ingest', ?6, ?6)",
        params![
            seg_id.clone(),
            workspace_id,
            module,
            parent_id,
            serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into()),
            now
        ],
    )
    .await
    .map_err(IngestError::Db)?;
    embedder.embed(workspace_id, &seg_id, text).await;
    Ok(seg_id)
}

/// Sets the parent entity's transcript rollup + emits `ingest.completed`.
/// Shared by the plain-text and audio completion paths.
async fn finish_completed(
    conn: &Connection,
    store: &lifeos_vcs::ObjectStore,
    workspace_id: &str,
    entity_id: &str,
    full_text: &str,
    segment_ids: Vec<String>,
    now: i64,
) -> Result<IngestOutcome, IngestError> {
    let transcript_ref = lifeos_vcs::store_blob(store, full_text.as_bytes()).map_err(IngestError::Io)?;
    conn.execute(
        "UPDATE entities SET attrs = json_set(attrs, '$.transcript_ref', ?1) WHERE id=?2",
        params![transcript_ref, entity_id],
    )
    .await
    .map_err(IngestError::Db)?;

    emit_event(
        conn,
        workspace_id,
        "ingest.completed",
        entity_id,
        &json!({ "segment_count": segment_ids.len() }),
        now,
    )
    .await
    .map_err(IngestError::Db)?;

    Ok(IngestOutcome::Completed { segment_ids })
}

/// The core, fully-testable orchestration `lifeos-drain` calls for every
/// claimed `ingest` job.
#[allow(clippy::too_many_arguments)]
pub async fn process_ingest_job(
    conn: &Connection,
    store: &lifeos_vcs::ObjectStore,
    embedder: &(dyn Embedder + Sync),
    transcriber: &(dyn Transcriber + Sync),
    captioner: &(dyn Captioner + Sync),
    ocr: &(dyn Ocr + Sync),
    workspace_id: &str,
    payload: IngestJobPayload,
    now: i64,
) -> Result<IngestOutcome, IngestError> {
    let entity_id = payload.entity_id.clone().ok_or(IngestError::MissingEntityId)?;
    let entity = fetch_entity(conn, workspace_id, &entity_id).await?;

    let blob_ref = payload
        .blob_ref
        .clone()
        .or_else(|| entity.blob_ref.clone())
        .ok_or(IngestError::NoContent)?;

    let name = entity_display_name(&entity, &payload);
    let route = route_by_mime(&name, payload.kind.as_deref());

    match route {
        MimeRoute::PlainText => {
            let bytes = lifeos_vcs::read_blob(store, &blob_ref).map_err(IngestError::Io)?;
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(_) => {
                    return finish_unsupported(conn, workspace_id, &entity_id, "binary", "no MIME route defined for this file type yet", now)
                        .await;
                }
            };

            let chunks = chunk_plain_text(&text);
            let mut segment_ids = Vec::with_capacity(chunks.len());
            for chunk in &chunks {
                let seg_id = insert_segment(
                    conn, embedder, workspace_id, &entity.module, &entity_id, chunk, None, None, None, now,
                )
                .await?;
                segment_ids.push(seg_id);
            }

            finish_completed(conn, store, workspace_id, &entity_id, &text, segment_ids, now).await
        }
        MimeRoute::Audio => {
            let bytes = lifeos_vcs::read_blob(store, &blob_ref).map_err(IngestError::Io)?;
            let samples = audio::decode_to_16k_mono_f32(&bytes).map_err(IngestError::Audio)?;
            let transcript_segments =
                transcriber.transcribe(&samples).await.map_err(IngestError::Transcription)?;

            let mut segment_ids = Vec::with_capacity(transcript_segments.len());
            let mut full_text = String::new();
            for seg in &transcript_segments {
                let seg_id = insert_segment(
                    conn,
                    embedder,
                    workspace_id,
                    &entity.module,
                    &entity_id,
                    &seg.text,
                    Some(seg.t_start_secs),
                    Some(seg.t_end_secs),
                    None,
                    now,
                )
                .await?;
                segment_ids.push(seg_id);
                if !full_text.is_empty() {
                    full_text.push(' ');
                }
                full_text.push_str(&seg.text);
            }

            finish_completed(conn, store, workspace_id, &entity_id, &full_text, segment_ids, now).await
        }
        MimeRoute::Image => {
            let bytes = lifeos_vcs::read_blob(store, &blob_ref).map_err(IngestError::Io)?;
            let mime = image_media_type(&name);
            let caption = captioner.caption(&bytes, mime).await.map_err(IngestError::Caption)?;
            let ocr_text = match ocr.extract_text(&bytes).await {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("lifeos-ingest: ocr failed for {entity_id}, continuing with caption only: {e}");
                    String::new()
                }
            };

            let mut segment_ids = Vec::with_capacity(2);
            let caption_seg_id = insert_segment(
                conn, embedder, workspace_id, &entity.module, &entity_id, &caption, None, None, None, now,
            )
            .await?;
            segment_ids.push(caption_seg_id);

            let mut full_text = caption.clone();
            let ocr_trimmed = ocr_text.trim();
            if !ocr_trimmed.is_empty() && ocr_trimmed != caption.trim() {
                let ocr_seg_id = insert_segment(
                    conn, embedder, workspace_id, &entity.module, &entity_id, ocr_trimmed, None, None, None, now,
                )
                .await?;
                segment_ids.push(ocr_seg_id);
                full_text.push_str("\n\n");
                full_text.push_str(ocr_trimmed);
            }

            finish_completed(conn, store, workspace_id, &entity_id, &full_text, segment_ids, now).await
        }
        MimeRoute::Pdf => {
            let bytes = lifeos_vcs::read_blob(store, &blob_ref).map_err(IngestError::Io)?;
            let pages = tokio::task::spawn_blocking(move || docs_extract::extract_pdf_pages(&bytes))
                .await
                .map_err(|e| IngestError::Extraction(format!("pdf extraction task panicked: {e}")))?
                .map_err(IngestError::Extraction)?;

            let mut segment_ids = Vec::new();
            let mut full_text = String::new();
            for (idx, page_text) in pages.iter().enumerate() {
                let trimmed = page_text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let seg_id = insert_segment(
                    conn,
                    embedder,
                    workspace_id,
                    &entity.module,
                    &entity_id,
                    trimmed,
                    None,
                    None,
                    Some((idx + 1) as u32),
                    now,
                )
                .await?;
                segment_ids.push(seg_id);
                if !full_text.is_empty() {
                    full_text.push_str("\n\n");
                }
                full_text.push_str(trimmed);
            }

            finish_completed(conn, store, workspace_id, &entity_id, &full_text, segment_ids, now).await
        }
        MimeRoute::Docx => {
            let bytes = lifeos_vcs::read_blob(store, &blob_ref).map_err(IngestError::Io)?;
            let text = docs_extract::extract_docx_text(&bytes).map_err(IngestError::Extraction)?;

            let chunks = chunk_plain_text(&text);
            let mut segment_ids = Vec::with_capacity(chunks.len());
            for chunk in &chunks {
                let seg_id = insert_segment(
                    conn, embedder, workspace_id, &entity.module, &entity_id, chunk, None, None, None, now,
                )
                .await?;
                segment_ids.push(seg_id);
            }

            finish_completed(conn, store, workspace_id, &entity_id, &text, segment_ids, now).await
        }
        MimeRoute::Unsupported { kind, blocked_by } => {
            finish_unsupported(conn, workspace_id, &entity_id, kind, blocked_by, now).await
        }
    }
}

/// Known, correctly-handled gap - not a failure. The job completes (nothing
/// to retry until #89/#90 land) and the parent entity records why, in the
/// same `{kind, blocked_by}` shape `/api/vcs/diff` already uses.
async fn finish_unsupported(
    conn: &Connection,
    workspace_id: &str,
    entity_id: &str,
    kind: &str,
    blocked_by: &str,
    now: i64,
) -> Result<IngestOutcome, IngestError> {
    conn.execute(
        "UPDATE entities SET attrs = json_set(attrs, '$.ingest_status', 'unsupported', '$.ingest_blocked_by', ?1) WHERE id=?2",
        params![blocked_by, entity_id],
    )
    .await
    .map_err(IngestError::Db)?;

    emit_event(
        conn,
        workspace_id,
        "ingest.unsupported",
        entity_id,
        &json!({ "kind": kind, "blocked_by": blocked_by }),
        now,
    )
    .await
    .map_err(IngestError::Db)?;

    Ok(IngestOutcome::Unsupported { kind: kind.to_string(), blocked_by: blocked_by.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use libsql::Builder;
    use lifeos_vcs::ObjectStore;

    async fn fresh_conn(path: &str) -> Connection {
        let _ = std::fs::remove_file(path);
        let db = Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE entities (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, module TEXT, type TEXT, \
                parent_id TEXT, title TEXT, attrs TEXT NOT NULL DEFAULT '{}', \
                source TEXT, blob_ref TEXT, created_at INTEGER, updated_at INTEGER)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE TABLE events (\
                id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, ts INTEGER, type TEXT, \
                entity_id TEXT, actor TEXT, attrs TEXT)",
            (),
        )
        .await
        .unwrap();
        conn
    }

    async fn insert_file_entity(conn: &Connection, id: &str, workspace_id: &str, name: &str, blob_ref: &str) {
        let attrs = json!({ "name": name });
        conn.execute(
            "INSERT INTO entities (id, workspace_id, module, type, attrs, blob_ref, created_at, updated_at) \
             VALUES (?1, ?2, 'files', 'file', ?3, ?4, 0, 0)",
            params![id, workspace_id, serde_json::to_string(&attrs).unwrap(), blob_ref],
        )
        .await
        .unwrap();
    }

    struct MockEmbedder {
        calls: Mutex<Vec<(String, String)>>,
    }

    impl MockEmbedder {
        fn new() -> Self {
            Self { calls: Mutex::new(Vec::new()) }
        }
    }

    #[async_trait]
    impl Embedder for MockEmbedder {
        async fn embed(&self, _workspace_id: &str, entity_id: &str, text: &str) {
            self.calls.lock().unwrap().push((entity_id.to_string(), text.to_string()));
        }
    }

    struct MockTranscriber {
        result: Result<Vec<TranscriptSegment>, String>,
    }

    #[async_trait]
    impl Transcriber for MockTranscriber {
        async fn transcribe(&self, _samples: &[f32]) -> Result<Vec<TranscriptSegment>, String> {
            self.result.clone()
        }
    }

    struct MockCaptioner {
        result: Result<String, String>,
    }

    #[async_trait]
    impl Captioner for MockCaptioner {
        async fn caption(&self, _image_bytes: &[u8], _mime: &str) -> Result<String, String> {
            self.result.clone()
        }
    }

    struct MockOcr {
        result: Result<String, String>,
    }

    #[async_trait]
    impl Ocr for MockOcr {
        async fn extract_text(&self, _image_bytes: &[u8]) -> Result<String, String> {
            self.result.clone()
        }
    }

    #[test]
    fn route_by_mime_dispatches_by_extension() {
        assert_eq!(route_by_mime("notes.txt", None), MimeRoute::PlainText);
        assert_eq!(route_by_mime("README.md", None), MimeRoute::PlainText);
        assert_eq!(route_by_mime("voicenote.mp3", None), MimeRoute::Audio);
        assert_eq!(route_by_mime("clip.wav", None), MimeRoute::Audio);
        assert_eq!(route_by_mime("voicenote.m4a", None), MimeRoute::Audio);
        match route_by_mime("clip.mov", None) {
            MimeRoute::Unsupported { kind, blocked_by } => {
                assert_eq!(kind, "video");
                assert!(!blocked_by.contains("#89"), "closed issue #89 should not be named as still-blocking");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
        assert_eq!(route_by_mime("cover.png", None), MimeRoute::Image);
        assert_eq!(route_by_mime("photo.jpg", None), MimeRoute::Image);
        assert_eq!(route_by_mime("doc.pdf", None), MimeRoute::Pdf);
        assert_eq!(route_by_mime("doc.docx", None), MimeRoute::Docx);
        match route_by_mime("weird.xyz", None) {
            MimeRoute::Unsupported { kind, blocked_by } => {
                assert_eq!(kind, "unknown");
                assert!(!blocked_by.is_empty());
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn chunk_plain_text_splits_on_blank_lines_and_drops_empties() {
        let chunks = chunk_plain_text("first para\n\n\n  second para  \n\n");
        assert_eq!(chunks, vec!["first para".to_string(), "second para".to_string()]);
        assert!(chunk_plain_text("   \n\n  ").is_empty());
    }

    #[tokio::test]
    async fn plain_text_produces_segments_transcript_ref_and_embeds() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let conn = fresh_conn("test_ingest_plain.db").await;

        let blob_ref = lifeos_vcs::store_blob(&store, b"para one\n\npara two").unwrap();
        insert_file_entity(&conn, "ent_file", "ws_1", "notes.txt", &blob_ref).await;

        let embedder = MockEmbedder::new();
        let transcriber = MockTranscriber { result: Ok(vec![]) };
        let payload = IngestJobPayload { entity_id: Some("ent_file".into()), ..Default::default() };
        let outcome = process_ingest_job(&conn, &store, &embedder, &transcriber, &NoopCaptioner, &NoopOcr, "ws_1", payload, 100).await.unwrap();

        let segment_ids = match outcome {
            IngestOutcome::Completed { segment_ids } => segment_ids,
            other => panic!("expected Completed, got {other:?}"),
        };
        assert_eq!(segment_ids.len(), 2);

        let mut rows = conn
            .query(
                "SELECT parent_id, type, json_extract(attrs,'$.text') FROM entities WHERE type='segment' ORDER BY id",
                (),
            )
            .await
            .unwrap();
        let mut seen = 0;
        while let Some(row) = rows.next().await.unwrap() {
            let parent_id: String = row.get(0).unwrap();
            let kind: String = row.get(1).unwrap();
            let text: String = row.get(2).unwrap();
            assert_eq!(parent_id, "ent_file");
            assert_eq!(kind, "segment");
            assert!(!text.is_empty());
            seen += 1;
        }
        assert_eq!(seen, 2);

        assert_eq!(embedder.calls.lock().unwrap().len(), 2);

        let mut parent_rows = conn
            .query("SELECT json_extract(attrs,'$.transcript_ref') FROM entities WHERE id='ent_file'", ())
            .await
            .unwrap();
        let transcript_ref: Option<String> = parent_rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert!(transcript_ref.is_some());

        let mut evt_rows = conn.query("SELECT type FROM events WHERE entity_id='ent_file'", ()).await.unwrap();
        let evt_type: String = evt_rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(evt_type, "ingest.completed");
    }

    #[tokio::test]
    async fn unsupported_mime_creates_no_segments_and_marks_the_parent() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let conn = fresh_conn("test_ingest_unsupported.db").await;

        let blob_ref = lifeos_vcs::store_blob(&store, b"fake video bytes").unwrap();
        insert_file_entity(&conn, "ent_clip", "ws_1", "clip.mp4", &blob_ref).await;

        let embedder = MockEmbedder::new();
        let transcriber = MockTranscriber { result: Ok(vec![]) };
        let payload = IngestJobPayload { entity_id: Some("ent_clip".into()), ..Default::default() };
        let outcome = process_ingest_job(&conn, &store, &embedder, &transcriber, &NoopCaptioner, &NoopOcr, "ws_1", payload, 100).await.unwrap();

        match outcome {
            IngestOutcome::Unsupported { kind, blocked_by } => {
                assert_eq!(kind, "video");
                assert!(!blocked_by.is_empty());
                assert!(!blocked_by.contains("#89"), "closed issue #89 should not be named as still-blocking");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }

        let mut seg_rows = conn.query("SELECT COUNT(*) FROM entities WHERE type='segment'", ()).await.unwrap();
        let count: i64 = seg_rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(count, 0);
        assert!(embedder.calls.lock().unwrap().is_empty());

        let mut parent_rows = conn
            .query(
                "SELECT json_extract(attrs,'$.ingest_status'), json_extract(attrs,'$.ingest_blocked_by') FROM entities WHERE id='ent_clip'",
                (),
            )
            .await
            .unwrap();
        let row = parent_rows.next().await.unwrap().unwrap();
        let status: String = row.get(0).unwrap();
        let blocked_by: String = row.get(1).unwrap();
        assert_eq!(status, "unsupported");
        assert!(!blocked_by.contains("#89"), "closed issue #89 should not be named as still-blocking");

        let mut evt_rows = conn.query("SELECT type FROM events WHERE entity_id='ent_clip'", ()).await.unwrap();
        let evt_type: String = evt_rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(evt_type, "ingest.unsupported");
    }

    #[tokio::test]
    async fn missing_entity_id_is_rejected_without_partial_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let conn = fresh_conn("test_ingest_missing_entity.db").await;

        let embedder = MockEmbedder::new();
        let transcriber = MockTranscriber { result: Ok(vec![]) };
        let payload = IngestJobPayload::default();
        let result = process_ingest_job(&conn, &store, &embedder, &transcriber, &NoopCaptioner, &NoopOcr, "ws_1", payload, 100).await;

        assert!(matches!(result, Err(IngestError::MissingEntityId)));
    }

    #[tokio::test]
    async fn missing_blob_ref_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let conn = fresh_conn("test_ingest_missing_blob.db").await;

        conn.execute(
            "INSERT INTO entities (id, workspace_id, module, type, attrs, created_at, updated_at) \
             VALUES ('ent_no_blob', 'ws_1', 'files', 'file', '{}', 0, 0)",
            (),
        )
        .await
        .unwrap();

        let embedder = MockEmbedder::new();
        let transcriber = MockTranscriber { result: Ok(vec![]) };
        let payload = IngestJobPayload { entity_id: Some("ent_no_blob".into()), ..Default::default() };
        let result = process_ingest_job(&conn, &store, &embedder, &transcriber, &NoopCaptioner, &NoopOcr, "ws_1", payload, 100).await;

        assert!(matches!(result, Err(IngestError::NoContent)));
    }

    fn synth_wav_blob() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = hound::WavWriter::new(&mut buf, spec).unwrap();
        for i in 0..8_000u32 {
            let t = i as f32 / 16_000.0;
            let sample = (t * 440.0 * std::f32::consts::TAU).sin() * i16::MAX as f32 * 0.5;
            writer.write_sample(sample as i16).unwrap();
        }
        writer.finalize().unwrap();
        buf.into_inner()
    }

    #[tokio::test]
    async fn audio_produces_timestamped_segments_transcript_ref_and_embeds() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let conn = fresh_conn("test_ingest_audio.db").await;

        let blob_ref = lifeos_vcs::store_blob(&store, &synth_wav_blob()).unwrap();
        insert_file_entity(&conn, "ent_clip", "ws_1", "voicenote.wav", &blob_ref).await;

        let embedder = MockEmbedder::new();
        let transcriber = MockTranscriber {
            result: Ok(vec![
                TranscriptSegment { text: "the treasure is buried".into(), t_start_secs: 0.0, t_end_secs: 1.2 },
                TranscriptSegment { text: "under the old oak tree".into(), t_start_secs: 1.2, t_end_secs: 2.5 },
            ]),
        };
        let payload = IngestJobPayload { entity_id: Some("ent_clip".into()), ..Default::default() };
        let outcome = process_ingest_job(&conn, &store, &embedder, &transcriber, &NoopCaptioner, &NoopOcr, "ws_1", payload, 100).await.unwrap();

        let segment_ids = match outcome {
            IngestOutcome::Completed { segment_ids } => segment_ids,
            other => panic!("expected Completed, got {other:?}"),
        };
        assert_eq!(segment_ids.len(), 2);

        let mut rows = conn
            .query(
                "SELECT json_extract(attrs,'$.text'), json_extract(attrs,'$.t_start'), json_extract(attrs,'$.t_end') \
                 FROM entities WHERE type='segment' ORDER BY json_extract(attrs,'$.t_start')",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let text: String = row.get(0).unwrap();
        let t_start: f64 = row.get(1).unwrap();
        let t_end: f64 = row.get(2).unwrap();
        assert_eq!(text, "the treasure is buried");
        assert_eq!(t_start, 0.0);
        assert_eq!(t_end, 1.2);

        assert_eq!(embedder.calls.lock().unwrap().len(), 2);

        let mut parent_rows = conn
            .query("SELECT json_extract(attrs,'$.transcript_ref') FROM entities WHERE id='ent_clip'", ())
            .await
            .unwrap();
        let transcript_ref: Option<String> = parent_rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert!(transcript_ref.is_some());

        let mut evt_rows = conn.query("SELECT type FROM events WHERE entity_id='ent_clip'", ()).await.unwrap();
        let evt_type: String = evt_rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(evt_type, "ingest.completed");
    }

    #[tokio::test]
    async fn transcription_failure_is_a_real_error_with_no_partial_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let conn = fresh_conn("test_ingest_audio_fail.db").await;

        let blob_ref = lifeos_vcs::store_blob(&store, &synth_wav_blob()).unwrap();
        insert_file_entity(&conn, "ent_clip2", "ws_1", "voicenote.wav", &blob_ref).await;

        let embedder = MockEmbedder::new();
        let transcriber = MockTranscriber { result: Err("no whisper model configured".into()) };
        let payload = IngestJobPayload { entity_id: Some("ent_clip2".into()), ..Default::default() };
        let result = process_ingest_job(&conn, &store, &embedder, &transcriber, &NoopCaptioner, &NoopOcr, "ws_1", payload, 100).await;

        assert!(matches!(result, Err(IngestError::Transcription(_))));

        let mut seg_rows = conn.query("SELECT COUNT(*) FROM entities WHERE type='segment'", ()).await.unwrap();
        let count: i64 = seg_rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(count, 0);
        assert!(embedder.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn image_produces_caption_and_ocr_segments_and_embeds() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let conn = fresh_conn("test_ingest_image.db").await;

        let blob_ref = lifeos_vcs::store_blob(&store, b"fake-png-bytes").unwrap();
        insert_file_entity(&conn, "ent_img", "ws_1", "cover.png", &blob_ref).await;

        let embedder = MockEmbedder::new();
        let transcriber = MockTranscriber { result: Ok(vec![]) };
        let captioner = MockCaptioner { result: Ok("a red bicycle leaning against a wall".into()) };
        let ocr = MockOcr { result: Ok("SALE 50% OFF".into()) };
        let payload = IngestJobPayload { entity_id: Some("ent_img".into()), ..Default::default() };
        let outcome =
            process_ingest_job(&conn, &store, &embedder, &transcriber, &captioner, &ocr, "ws_1", payload, 100)
                .await
                .unwrap();

        let segment_ids = match outcome {
            IngestOutcome::Completed { segment_ids } => segment_ids,
            other => panic!("expected Completed, got {other:?}"),
        };
        assert_eq!(segment_ids.len(), 2);

        let mut rows = conn
            .query("SELECT json_extract(attrs,'$.text') FROM entities WHERE type='segment' ORDER BY id", ())
            .await
            .unwrap();
        let mut texts = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            let text: String = row.get(0).unwrap();
            texts.push(text);
        }
        assert!(texts.iter().any(|t| t.contains("bicycle")));
        assert!(texts.iter().any(|t| t.contains("SALE 50% OFF")));
        assert_eq!(embedder.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn image_caption_failure_is_a_real_error_ocr_failure_degrades() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let conn = fresh_conn("test_ingest_image_fail.db").await;

        let blob_ref = lifeos_vcs::store_blob(&store, b"fake-png-bytes").unwrap();
        insert_file_entity(&conn, "ent_img_fail", "ws_1", "cover.png", &blob_ref).await;

        let embedder = MockEmbedder::new();
        let transcriber = MockTranscriber { result: Ok(vec![]) };
        let captioner = MockCaptioner { result: Err("no vision captioner configured".into()) };
        let ocr = MockOcr { result: Ok(String::new()) };
        let payload = IngestJobPayload { entity_id: Some("ent_img_fail".into()), ..Default::default() };
        let result =
            process_ingest_job(&conn, &store, &embedder, &transcriber, &captioner, &ocr, "ws_1", payload, 100).await;
        assert!(matches!(result, Err(IngestError::Caption(_))));

        // OCR failing must not fail the job - captioning alone still makes the image searchable.
        let conn2 = fresh_conn("test_ingest_image_ocr_degrade.db").await;
        insert_file_entity(&conn2, "ent_img_ocr", "ws_1", "cover.png", &blob_ref).await;
        let captioner2 = MockCaptioner { result: Ok("a caption".into()) };
        let ocr2 = MockOcr { result: Err("tesseract not found".into()) };
        let payload2 = IngestJobPayload { entity_id: Some("ent_img_ocr".into()), ..Default::default() };
        let outcome2 = process_ingest_job(
            &conn2, &store, &embedder, &transcriber, &captioner2, &ocr2, "ws_1", payload2, 100,
        )
        .await
        .unwrap();
        match outcome2 {
            IngestOutcome::Completed { segment_ids } => assert_eq!(segment_ids.len(), 1),
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    /// Builds a minimal single-content-stream PDF with real, byte-accurate
    /// xref offsets (computed here rather than hardcoded) so `pdf-extract`
    /// can parse it without a binary fixture file.
    fn synth_pdf_bytes(page_texts: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut offsets = Vec::new();
        buf.extend_from_slice(b"%PDF-1.4\n");

        offsets.push(buf.len());
        buf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let page_obj_start = 3;
        let kids: String =
            (0..page_texts.len()).map(|i| format!("{} 0 R ", page_obj_start + i)).collect::<Vec<_>>().join("");
        offsets.push(buf.len());
        buf.extend_from_slice(
            format!("2 0 obj\n<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n", kids.trim(), page_texts.len())
                .as_bytes(),
        );

        let font_obj = page_obj_start + page_texts.len();
        for i in 0..page_texts.len() {
            let obj_num = page_obj_start + i;
            offsets.push(buf.len());
            buf.extend_from_slice(
                format!(
                    "{obj_num} 0 obj\n<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 {font_obj} 0 R >> >> /MediaBox [0 0 612 792] /Contents {} 0 R >>\nendobj\n",
                    font_obj + 1 + i
                )
                .as_bytes(),
            );
        }

        offsets.push(buf.len());
        buf.extend_from_slice(
            format!("{font_obj} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n").as_bytes(),
        );

        for (i, text) in page_texts.iter().enumerate() {
            let obj_num = font_obj + 1 + i;
            let content = format!("BT /F1 24 Tf 72 712 Td ({text}) Tj ET");
            offsets.push(buf.len());
            buf.extend_from_slice(
                format!("{obj_num} 0 obj\n<< /Length {} >>\nstream\n{content}\nendstream\nendobj\n", content.len())
                    .as_bytes(),
            );
        }

        let xref_start = buf.len();
        let total_objs = offsets.len() + 1;
        buf.extend_from_slice(format!("xref\n0 {total_objs}\n").as_bytes());
        buf.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            buf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        buf.extend_from_slice(
            format!(
                "trailer\n<< /Size {total_objs} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF",
            )
            .as_bytes(),
        );
        buf
    }

    #[tokio::test]
    async fn pdf_produces_per_page_segments_with_page_locator() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let conn = fresh_conn("test_ingest_pdf.db").await;

        let blob_ref = lifeos_vcs::store_blob(&store, &synth_pdf_bytes(&["Hello PDF", "Second page text"])).unwrap();
        insert_file_entity(&conn, "ent_pdf", "ws_1", "report.pdf", &blob_ref).await;

        let embedder = MockEmbedder::new();
        let transcriber = MockTranscriber { result: Ok(vec![]) };
        let payload = IngestJobPayload { entity_id: Some("ent_pdf".into()), ..Default::default() };
        let outcome = process_ingest_job(
            &conn, &store, &embedder, &transcriber, &NoopCaptioner, &NoopOcr, "ws_1", payload, 100,
        )
        .await
        .unwrap();

        let segment_ids = match outcome {
            IngestOutcome::Completed { segment_ids } => segment_ids,
            other => panic!("expected Completed, got {other:?}"),
        };
        assert_eq!(segment_ids.len(), 2);

        let mut rows = conn
            .query(
                "SELECT json_extract(attrs,'$.page'), json_extract(attrs,'$.text') FROM entities \
                 WHERE type='segment' ORDER BY json_extract(attrs,'$.page')",
                (),
            )
            .await
            .unwrap();
        let row1 = rows.next().await.unwrap().unwrap();
        let page1: i64 = row1.get(0).unwrap();
        let text1: String = row1.get(1).unwrap();
        assert_eq!(page1, 1);
        assert!(text1.contains("Hello PDF"));

        let row2 = rows.next().await.unwrap().unwrap();
        let page2: i64 = row2.get(0).unwrap();
        assert_eq!(page2, 2);
    }

    #[tokio::test]
    async fn docx_produces_paragraph_segments() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let conn = fresh_conn("test_ingest_docx.db").await;

        let bytes = docs_extract::tests_support::synth_docx_bytes(&["First paragraph", "Second paragraph"]);
        let blob_ref = lifeos_vcs::store_blob(&store, &bytes).unwrap();
        insert_file_entity(&conn, "ent_docx", "ws_1", "notes.docx", &blob_ref).await;

        let embedder = MockEmbedder::new();
        let transcriber = MockTranscriber { result: Ok(vec![]) };
        let payload = IngestJobPayload { entity_id: Some("ent_docx".into()), ..Default::default() };
        let outcome = process_ingest_job(
            &conn, &store, &embedder, &transcriber, &NoopCaptioner, &NoopOcr, "ws_1", payload, 100,
        )
        .await
        .unwrap();

        let segment_ids = match outcome {
            IngestOutcome::Completed { segment_ids } => segment_ids,
            other => panic!("expected Completed, got {other:?}"),
        };
        assert_eq!(segment_ids.len(), 2);
    }

    fn tiny_wav_bytes() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(&mut buf, spec).unwrap();
        for i in 0..1_600 {
            writer.write_sample(((i as f32 * 0.1).sin() * 1000.0) as i16).unwrap();
        }
        writer.finalize().unwrap();
        buf.into_inner()
    }

    #[tokio::test]
    async fn transcribe_audio_bytes_joins_segment_texts_into_one_transcript() {
        let transcriber = MockTranscriber {
            result: Ok(vec![
                TranscriptSegment { text: "hello".into(), t_start_secs: 0.0, t_end_secs: 1.0 },
                TranscriptSegment { text: "world".into(), t_start_secs: 1.0, t_end_secs: 2.0 },
            ]),
        };
        // A symphonia-decodable WAV needs no ffmpeg fallback.
        let transcript = transcribe_audio_bytes(&tiny_wav_bytes(), &transcriber, None).await.unwrap();
        assert_eq!(transcript, "hello world");
    }

    #[tokio::test]
    async fn transcribe_audio_bytes_surfaces_the_decode_error_for_ogg_without_ffmpeg() {
        let transcriber = MockTranscriber { result: Ok(vec![]) };
        let err = transcribe_audio_bytes(b"OggS\x00\x02 fake opus", &transcriber, None).await.unwrap_err();
        assert!(err.contains("symphonia") || err.to_lowercase().contains("container"), "got: {err}");
    }
}
