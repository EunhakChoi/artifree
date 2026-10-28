//! Adapter-neutral artifact operations. Each function converts wire values
//! into domain values, runs one `ArtifactService` command off the async
//! executor, and converts the result back. HTTP and MCP both call these
//! functions, so they share validation, authorization, conflict, retry, and
//! error semantics; only the framing differs.

use artifree_contracts::api::{
    Artifact, ArtifactKind as WireKind, ArtifactList, CommitResponse, ContentEncoding, CopiedPart,
    CreateArtifactRequest, DuplicateArtifactRequest, DuplicateArtifactResponse, ErrorBody,
    ErrorCode, ExportRuntime, FileChange as WireChange, FileUpload, ListArtifactsQuery, ListSort,
    Preview, RestoreRevisionRequest, Revision, RevisionFile, RevisionList, SaveRevisionRequest,
    SourceExportFormat, SourceExportManifest, UpdateArtifactRequest, UpdateArtifactResponse,
};
use artifree_core::artifacts::{ArtifactError, MetadataChange, Missing, RevisionExport};
use artifree_core::listing::{ListQuery, PageSize, SearchText};
use artifree_core::parts::{Boundary, ByteRange, FilePart, RangeError};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title};
use artifree_domain::package::{
    ENTRY_PATH, FileChange, FilePackage, MAX_CHANGES, MAX_FILES, PackagePath,
};
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactRecord, CommitReceipt, Delivery, ListOrder, RevisionRecord,
};
use artifree_providers::archive::{self, ArchiveEntry};
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::auth::hex;
use crate::http::AppState;

/// A failed operation: the HTTP status and the contract error body. MCP
/// reports the same body in an error tool result.
#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) body: ErrorBody,
    /// For `range_not_satisfiable`, the file's size, which HTTP reports in
    /// `Content-Range`.
    pub(crate) file_size: Option<u64>,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody {
                code,
                message: message.into(),
                current_revision: None,
                current_metadata_revision: None,
            },
            file_size: None,
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ErrorCode::InvalidRequest, message)
    }

    pub(crate) fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::InvalidRequest,
            message,
        )
    }

    pub(crate) fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthenticated,
            "a valid bearer access token is required",
        )
    }
}

impl From<ArtifactError> for ApiError {
    fn from(error: ArtifactError) -> Self {
        let message = error.to_string();
        match error {
            ArtifactError::InvalidPackage(_) => Self::unprocessable(message),
            ArtifactError::NotFound(_) => {
                Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
            }
            ArtifactError::RevisionConflict { current } => {
                let mut conflict =
                    Self::new(StatusCode::CONFLICT, ErrorCode::RevisionConflict, message);
                conflict.body.current_revision = Some(current.0);
                conflict
            }
            ArtifactError::MetadataConflict { current } => {
                let mut conflict =
                    Self::new(StatusCode::CONFLICT, ErrorCode::MetadataConflict, message);
                conflict.body.current_metadata_revision = Some(current.0);
                conflict
            }
            ArtifactError::InvalidPageToken => Self::invalid(format!("page_token: {message}")),
            ArtifactError::InvalidRange(RangeError::PastEnd { size }) => {
                let mut refusal = Self::new(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    ErrorCode::RangeNotSatisfiable,
                    message,
                );
                refusal.file_size = Some(size);
                refusal
            }
            ArtifactError::InvalidRange(_) => Self::invalid(message),
            ArtifactError::RequestKeyReused => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::RequestKeyReused,
                message,
            ),
            ArtifactError::Unavailable(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                message,
            ),
            _ => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                message,
            ),
        }
    }
}

/// The bytes of one file of a pinned revision, whole or a part.
#[derive(Debug)]
pub(crate) struct FileBytes {
    pub(crate) path: PackagePath,
    pub(crate) revision: SourceRevision,
    pub(crate) part: FilePart,
}

/// Parses a retry key; `field` names where it came from.
pub(crate) fn request_key(field: &str, value: &str) -> Result<RequestKey, ApiError> {
    value
        .parse()
        .map_err(|error| ApiError::invalid(format!("{field}: {error}")))
}

pub(crate) async fn list(
    state: &AppState,
    actor: ActorId,
    query: ListArtifactsQuery,
) -> Result<ArtifactList, ApiError> {
    let ListArtifactsQuery {
        q,
        sort,
        pinned,
        deleted,
        limit,
        page_token,
    } = query;
    let search: Option<SearchText> = q
        .map(|text| text.parse())
        .transpose()
        .map_err(|error| ApiError::invalid(format!("q: {error}")))?;
    let limit = limit
        .map(PageSize::try_from)
        .transpose()
        .map_err(|error| ApiError::invalid(format!("limit: {error}")))?
        .unwrap_or_default();
    let query = ListQuery {
        search,
        pinned,
        deleted: deleted.unwrap_or(false),
        order: match sort.unwrap_or(ListSort::Updated) {
            ListSort::Updated => ListOrder::Updated,
            ListSort::Created => ListOrder::Created,
        },
        limit,
    };
    let page = state
        .run(move |artifacts| artifacts.list(&actor, &query, page_token.as_deref()))
        .await?;
    Ok(ArtifactList {
        artifacts: page.artifacts.iter().map(artifact_dto).collect(),
        next_page_token: page.next_page_token,
    })
}

pub(crate) async fn show(state: &AppState, actor: ActorId, id: &str) -> Result<Artifact, ApiError> {
    let id = artifact_id(id)?;
    let record = state
        .run(move |artifacts| artifacts.artifact(&actor, &id))
        .await?;
    Ok(artifact_dto(&record))
}

pub(crate) async fn create(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    request: CreateArtifactRequest,
) -> Result<CommitResponse, ApiError> {
    let title: Title = request
        .title
        .parse()
        .map_err(|error| ApiError::unprocessable(format!("title: {error}")))?;
    too_many("files", request.files.len(), MAX_FILES)?;
    let mut files = Vec::with_capacity(request.files.len());
    for (
        index,
        FileUpload {
            path,
            encoding,
            content,
        },
    ) in request.files.into_iter().enumerate()
    {
        let path = package_path(&format!("files[{index}].path"), &path)?;
        files.push((
            path,
            decode(&format!("files[{index}].content"), encoding, content)?,
        ));
    }
    let package = FilePackage::new(files)
        .map_err(|error| ApiError::unprocessable(format!("files: {error}")))?;
    let receipt = state
        .run(move |artifacts| artifacts.create(&actor, &key, &title, &package))
        .await?;
    Ok(announce(state, receipt))
}

pub(crate) async fn save(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    request: SaveRevisionRequest,
) -> Result<CommitResponse, ApiError> {
    let id = artifact_id(id)?;
    let base = SourceRevision(request.base_revision);
    too_many("changes", request.changes.len(), MAX_CHANGES)?;
    let mut changes = Vec::with_capacity(request.changes.len());
    for (index, change) in request.changes.into_iter().enumerate() {
        changes.push(match change {
            WireChange::Put {
                path,
                encoding,
                content,
            } => FileChange::Put {
                path: package_path(&format!("changes[{index}].path"), &path)?,
                content: decode(&format!("changes[{index}].content"), encoding, content)?,
            },
            WireChange::Delete { path } => FileChange::Delete {
                path: package_path(&format!("changes[{index}].path"), &path)?,
            },
        });
    }
    let receipt = state
        .run(move |artifacts| artifacts.save(&actor, &key, &id, base, changes))
        .await?;
    Ok(announce(state, receipt))
}

pub(crate) async fn restore(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    revision: u64,
    request: RestoreRevisionRequest,
) -> Result<CommitResponse, ApiError> {
    let id = artifact_id(id)?;
    let revision = revision_number(revision)?;
    let base = SourceRevision(request.base_revision);
    let receipt = state
        .run(move |artifacts| artifacts.restore(&actor, &key, &id, base, revision))
        .await?;
    Ok(announce(state, receipt))
}

pub(crate) async fn duplicate(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    revision: u64,
    request: DuplicateArtifactRequest,
) -> Result<DuplicateArtifactResponse, ApiError> {
    let id = artifact_id(id)?;
    let revision = revision_number(revision)?;
    let title: Option<Title> = request
        .title
        .map(|title| title.parse())
        .transpose()
        .map_err(|error| ApiError::unprocessable(format!("title: {error}")))?;
    let receipt = state
        .run(move |artifacts| artifacts.duplicate(&actor, &key, &id, revision, title.as_ref()))
        .await?;
    let CommitResponse {
        artifact,
        revision,
        replayed,
    } = announce(state, receipt);
    Ok(DuplicateArtifactResponse {
        artifact,
        revision,
        copied: vec![CopiedPart::Files],
        replayed,
    })
}

pub(crate) async fn update(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    request: UpdateArtifactRequest,
) -> Result<UpdateArtifactResponse, ApiError> {
    let id = artifact_id(id)?;
    let base = MetadataRevision(request.base_metadata_revision);
    let title: Option<Title> = request
        .title
        .map(|title| title.parse())
        .transpose()
        .map_err(|error| ApiError::unprocessable(format!("title: {error}")))?;
    let change = MetadataChange::new(title, request.pinned, request.deleted)
        .ok_or_else(|| ApiError::unprocessable("give a title, pinned, deleted, or several"))?;
    let receipt = state
        .run(move |artifacts| artifacts.update(&actor, &key, &id, base, &change))
        .await?;
    if receipt.delivery == Delivery::Committed {
        state.feed().committed();
    }
    Ok(UpdateArtifactResponse {
        artifact: artifact_dto(&receipt.artifact),
        replayed: receipt.delivery == Delivery::Replayed,
    })
}

/// Wakes change streams when the commit wrote something, and converts the
/// receipt.
fn announce(state: &AppState, receipt: CommitReceipt) -> CommitResponse {
    if receipt.delivery == Delivery::Committed {
        state.feed().committed();
    }
    commit_response(receipt)
}

pub(crate) async fn revisions(
    state: &AppState,
    actor: ActorId,
    id: &str,
) -> Result<RevisionList, ApiError> {
    let id = artifact_id(id)?;
    let revisions = state
        .run(move |artifacts| artifacts.revisions(&actor, &id))
        .await?;
    Ok(RevisionList {
        revisions: revisions.into_iter().map(revision_dto).collect(),
    })
}

pub(crate) async fn revision(
    state: &AppState,
    actor: ActorId,
    id: &str,
    revision: u64,
) -> Result<Revision, ApiError> {
    let id = artifact_id(id)?;
    let revision = revision_number(revision)?;
    let record = state
        .run(move |artifacts| artifacts.revision(&actor, &id, revision))
        .await?;
    Ok(revision_dto(record))
}

pub(crate) async fn read_file(
    state: &AppState,
    actor: ActorId,
    id: &str,
    revision: u64,
    path: &str,
    range: ByteRange,
    boundary: Boundary,
) -> Result<FileBytes, ApiError> {
    let id = artifact_id(id)?;
    let revision = revision_number(revision)?;
    let path = package_path("path", path)?;
    let read_path = path.clone();
    let part = state
        .run(move |artifacts| {
            artifacts.read_file_part(&actor, &id, revision, &read_path, range, boundary)
        })
        .await?;
    Ok(FileBytes {
        path,
        revision,
        part,
    })
}

/// The name of the manifest inside a source export (decision 0015). A
/// package path never starts with a dot, so no exported file has it.
pub(crate) const EXPORT_MANIFEST: &str = ".artifree-export.json";

/// A source export: a ZIP archive and the name to save it under.
pub(crate) struct SourceExport {
    pub(crate) file_name: String,
    pub(crate) archive: Vec<u8>,
}

pub(crate) async fn export(
    state: &AppState,
    actor: ActorId,
    id: &str,
    revision: u64,
) -> Result<SourceExport, ApiError> {
    let id = artifact_id(id)?;
    let revision = revision_number(revision)?;
    let file_name = format!("{id}-r{}.zip", revision.0);
    let archive = state
        .run(move |artifacts| {
            // Package limits keep an archive far within the ZIP format's, so
            // this failure is not expected; it is answered as unavailable.
            source_archive(artifacts.export(&actor, &id, revision)?)
                .map_err(ArtifactError::Unavailable)
        })
        .await?;
    Ok(SourceExport { file_name, archive })
}

/// The manifest first, then every file at its package path, all dated at
/// the revision's commit.
fn source_archive(export: RevisionExport) -> Result<Vec<u8>, String> {
    let modified = export.revision.created_at;
    let revision = revision_dto(export.revision);
    let manifest = SourceExportManifest {
        format: SourceExportFormat::V1,
        artifact_id: export.artifact.id.to_string(),
        title: export.artifact.title.as_str().to_owned(),
        revision: revision.revision,
        created_at_ms: revision.created_at_ms,
        entry: ENTRY_PATH.to_owned(),
        runtime: ExportRuntime::None,
        files: revision.files,
    };
    let mut manifest = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("could not write the export manifest: {error}"))?;
    manifest.push(b'\n');
    let files = export
        .files
        .into_files()
        .map(|(path, content)| ArchiveEntry {
            name: path.to_string(),
            content,
        });
    let entries = std::iter::once(ArchiveEntry {
        name: EXPORT_MANIFEST.to_owned(),
        content: manifest,
    })
    .chain(files);
    archive::stored_zip(entries, modified).map_err(|error| error.to_string())
}

pub(crate) async fn preview(
    state: &AppState,
    actor: ActorId,
    id: &str,
    revision: u64,
) -> Result<Preview, ApiError> {
    let id = artifact_id(id)?;
    let revision = revision_number(revision)?;
    let lookup_actor = actor.clone();
    let lookup_id = id.clone();
    let record = state
        .run(move |artifacts| artifacts.artifact(&lookup_actor, &lookup_id))
        .await?;
    if revision > record.current_revision {
        return Err(ArtifactError::NotFound(Missing::Revision).into());
    }
    let ticket = state.previews().issue(actor, id, revision).ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "too many live previews; retry shortly",
        )
    })?;
    Ok(Preview {
        url: format!("{}/p/{}/", state.settings().content_origin, ticket.secret),
        revision: revision.0,
        expires_at_ms: ticket.expires_at_ms,
    })
}

// An identifier that cannot exist is reported like one the actor cannot
// access, so callers cannot tell the two apart.
pub(crate) fn artifact_id(value: &str) -> Result<ArtifactId, ApiError> {
    value.parse().map_err(|_| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "artifact not found",
        )
    })
}

pub(crate) fn revision_number(value: u64) -> Result<SourceRevision, ApiError> {
    if value == 0 {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "revision not found",
        ));
    }
    Ok(SourceRevision(value))
}

pub(crate) fn package_path(field: &str, value: &str) -> Result<PackagePath, ApiError> {
    value
        .parse()
        .map_err(|error| ApiError::unprocessable(format!("{field}: {error}")))
}

fn decode(field: &str, encoding: ContentEncoding, content: String) -> Result<Vec<u8>, ApiError> {
    match encoding {
        ContentEncoding::Utf8 => Ok(content.into_bytes()),
        ContentEncoding::Base64 => BASE64
            .decode(content)
            .map_err(|error| ApiError::invalid(format!("{field}: invalid base64: {error}"))),
    }
}

/// Rejects an oversized list before its entries are decoded. `limit` is
/// [`MAX_FILES`] for a new package and [`MAX_CHANGES`] for a change set; the
/// domain validates the resulting package either way.
fn too_many(what: &str, count: usize, limit: usize) -> Result<(), ApiError> {
    if count > limit {
        return Err(ApiError::unprocessable(format!(
            "at most {limit} {what} are allowed"
        )));
    }
    Ok(())
}

fn commit_response(receipt: CommitReceipt) -> CommitResponse {
    CommitResponse {
        artifact: artifact_dto(&receipt.artifact),
        revision: revision_dto(receipt.revision),
        replayed: receipt.delivery == Delivery::Replayed,
    }
}

fn artifact_dto(record: &ArtifactRecord) -> Artifact {
    Artifact {
        id: record.id.to_string(),
        kind: match record.kind {
            ArtifactKind::App => WireKind::App,
            ArtifactKind::Document => WireKind::Document,
            ArtifactKind::Slides => WireKind::Slides,
            ArtifactKind::Dashboard => WireKind::Dashboard,
            ArtifactKind::Design => WireKind::Design,
            ArtifactKind::Motion => WireKind::Motion,
        },
        title: record.title.as_str().to_owned(),
        pinned: record.pinned,
        current_revision: record.current_revision.0,
        metadata_revision: record.metadata_revision.0,
        deleted_at_ms: record.deleted_at.map(|time| time.0),
        created_at_ms: record.created_at.0,
        updated_at_ms: record.updated_at.0,
    }
}

fn revision_dto(record: RevisionRecord) -> Revision {
    Revision {
        revision: record.revision.0,
        created_by: record.created_by.to_string(),
        created_at_ms: record.created_at.0,
        files: record
            .files
            .into_iter()
            .map(|entry| RevisionFile {
                media_type: entry.path.media_type().essence().to_owned(),
                path: entry.path.to_string(),
                size: entry.size,
                sha256: hex(&entry.sha256),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use artifree_domain::identity::UnixMillis;
    use artifree_ports::artifact_store::FileEntry;

    use super::*;

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn u32_at(bytes: &[u8], at: usize) -> Result<usize, Box<dyn Error>> {
        let value = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        Ok(usize::try_from(value)?)
    }

    #[test]
    fn an_archive_holds_the_manifest_then_the_files_dated_at_the_commit()
    -> Result<(), Box<dyn Error>> {
        let page = b"<p>2</p>".to_vec();
        // Created long before the revision, so the two times differ.
        let artifact = ArtifactRecord {
            id: format!("art_{}", "a".repeat(32)).parse()?,
            owner: "owner".parse()?,
            kind: ArtifactKind::App,
            title: "Export".parse()?,
            pinned: false,
            current_revision: SourceRevision(2),
            metadata_revision: MetadataRevision(1),
            deleted_at: None,
            created_at: UnixMillis(0),
            updated_at: UnixMillis(1_793_180_967_999),
        };
        let revision = RevisionRecord {
            revision: SourceRevision(2),
            created_by: "owner".parse()?,
            // 2026-10-28T09:49:27.999Z
            created_at: UnixMillis(1_793_180_967_999),
            files: vec![FileEntry {
                path: PackagePath::entry(),
                size: 8,
                sha256: [7; 32],
            }],
        };
        let files = FilePackage::new(vec![(PackagePath::entry(), page.clone())])?;
        let archive = source_archive(RevisionExport {
            artifact,
            revision,
            files,
        })?;

        let mut entries = Vec::new();
        let mut at = 0;
        while u32_at(&archive, at)? == 0x0403_4b50 {
            let size = u32_at(&archive, at + 18)?;
            let name_length = usize::from(u16_at(&archive, at + 26));
            let start = at + 30 + name_length;
            let name = std::str::from_utf8(&archive[at + 30..start])?.to_owned();
            let (time, date) = (u16_at(&archive, at + 10), u16_at(&archive, at + 12));
            entries.push((name, archive[start..start + size].to_vec(), time, date));
            at = start + size;
        }
        let names: Vec<&str> = entries.iter().map(|entry| entry.0.as_str()).collect();
        assert_eq!(names, [EXPORT_MANIFEST, "index.html"]);
        assert_eq!(entries[1].1, page);
        for (name, _, time, date) in &entries {
            assert_eq!(
                (1980 + (date >> 9), (date >> 5) & 0xF, date & 0x1F),
                (2026, 10, 28),
                "{name}"
            );
            assert_eq!((time >> 11, (time >> 5) & 0x3F), (9, 49), "{name}");
        }
        let manifest: SourceExportManifest = serde_json::from_slice(&entries[0].1)?;
        assert_eq!(
            (manifest.revision, manifest.created_at_ms, manifest.runtime),
            (2, 1_793_180_967_999, ExportRuntime::None)
        );
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].sha256, "07".repeat(32));
        Ok(())
    }
}
