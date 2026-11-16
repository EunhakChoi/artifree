//! Adapter-neutral artifact operations. Each function converts wire values
//! into domain values, runs one `ArtifactService` command off the async
//! executor, and converts the result back. HTTP and MCP both call these
//! functions, so they share validation, authorization, conflict, retry, and
//! error semantics; only the framing differs.

use artifree_contracts::api::{
    AccessUser, AgentSessionList, AppData, Artifact, ArtifactKind as WireKind, ArtifactList,
    ArtifactTypeRef, Asset, AssetList, BackgroundStatus, BuildStatus as WireBuildStatus,
    CallerRole, CancelJobResponse, CommitResponse, ConflictingFile, ContentEncoding, CopiedPart,
    CreateArtifactRequest, CreateRuntimeViewRequest, DataMembers, DataSpaceName, DataWriteResponse,
    DeleteAssetRequest, DeleteAssetResponse, DeleteDataRequest, DiagnosticLocation,
    DuplicateArtifactRequest, DuplicateArtifactResponse, EditFilesRequest, EnqueueJobRequest,
    EnqueueJobResponse, ErrorBody, ErrorCode, ExecutionStatus, ExportRuntime,
    FileChange as WireChange, FileEdit as WireEdit, FileUpload, Handoff, HandoffState,
    HeadBuildStatus, Job as WireJob, JobBuild, JobDiagnostic, JobFailure as WireFailure,
    JobKind as WireJobKind, JobList, JobMismatch, JobProblem, JobResult, JobState as WireJobState,
    JobVerdict, JsonValue, ListAccess as WireListAccess, ListArtifactsQuery, ListJobsQuery,
    ListSort, Preview, PurgeArtifactRequest, PurgeArtifactResponse, ReadDataMembersQuery,
    RemoveUnreferencedContentRequest, RemoveUnreferencedContentResponse, RestoreRevisionRequest,
    Revision, RevisionFile, RevisionList, RuntimeConnection, RuntimeGrant, RuntimeMetadata,
    RuntimeView, SaveRevisionRequest, SetDataRequest, SetRuntimeGrantRequest, SourceExportFormat,
    SourceExportManifest, StageHandoffRequest, TYPE_NOTICE, TypeDescriptor, TypeList,
    TypeReference, TypeSummary, UnreferencedContent as WireContent, UpdateArtifactRequest,
    UpdateArtifactResponse, UpdateDataRequest, UploadAssetRequest, UploadAssetResponse,
};
use artifree_contracts::mcp::{HandoffInbox, HandoffReceipt, RegisteredHandoffSession};
use artifree_core::artifacts::{
    ArtifactError, ArtifactService, CopyParts, MetadataChange, Missing, RevisionExport,
};
use artifree_core::assets::AssetUpload;
use artifree_core::build_reads::HeadBuild;
use artifree_core::catalog::Release;
use artifree_core::data::DataMutation;
use artifree_core::jobs::{JobPageSize, NewBuild};
use artifree_core::listing::{ListQuery, PageArtifact, PageSize, SearchText};
use artifree_core::members::{MAX_TOKEN_BYTES, MemberLimit, MemberQuery, MemberToken};
use artifree_core::parts::{Boundary, ByteRange, FilePart, RangeError};
use artifree_core::room::TabKey;
use artifree_domain::AppDataVersion;
use artifree_domain::access::{Access, AccessGeneration, Need, Role, Space};
use artifree_domain::artifact_type::{ArtifactType, TypeId, TypeRelease};
use artifree_domain::assets::{
    AssetId, AssetMediaType, AssetMetadata, AssetRejection, AssetVersion, MAX_ASSET_BYTES,
    MAX_SPACE_ASSETS, MAX_SPACE_BYTES,
};
use artifree_domain::data::{DataChange, DataError, DataPath, DataValue};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, StateGeneration, Title, TokenId};
use artifree_domain::job::{
    BuildDiagnostic, JobFailure, JobId, JobKind, JobState, Problem, ProfileName, is_public_job,
};
use artifree_domain::package::{
    ENTRY_PATH, FileChange, FilePackage, FileSet, MAX_CHANGES, MAX_FILES, PackagePath,
};
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactRecord, CommitReceipt, ContentSetDigest, Delivery, FileEdit, FileExpectation,
    FileMismatch, ListAccess, ListOrder, RevisionRecord, UnreferencedContent, parse_sha256,
};
use artifree_ports::asset_store::{AssetContent, AssetStoreError};
use artifree_ports::data_store::OutboxFence;
use artifree_ports::job_store::JobRecord;
use artifree_providers::archive::{self, ArchiveEntry};
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD};

use crate::auth::hex;
use crate::changes::Feed;
use crate::handoffs::{self, HandoffError};
use crate::http::AppState;
use crate::preview::{
    self, PreviewTarget, RuntimeAuthority, RuntimeIssueError, RuntimeTicketRequest, ViewKey,
};
use crate::runtime_policy::{Capability, CapabilitySet, MANIFEST_PATH, RuntimePolicy, SDK_PIN};
use crate::runtime_sessions::{GrantScope, SessionError};

/// A failed operation: the HTTP status and the contract error body. MCP
/// reports the same body in an error tool result.
#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    /// Boxed so that a `Result` carrying an error stays small.
    pub(crate) body: Box<ErrorBody>,
    /// For `range_not_satisfiable`, the file's size, which HTTP states in
    /// `Content-Range`.
    pub(crate) range_size: Option<u64>,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: Box::new(ErrorBody {
                code,
                message: message.into(),
                current_revision: None,
                current_metadata_revision: None,
                current_access_version: None,
                current_data_version: None,
                current_asset_version: None,
                conflicting_files: None,
                conflicting_files_total: None,
                missing_paths: None,
            }),
            range_size: None,
        }
    }

    /// A range that selects no bytes of a file of `size` bytes.
    pub(crate) fn unsatisfiable(size: u64, message: impl Into<String>) -> Self {
        let mut error = Self::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            ErrorCode::RangeNotSatisfiable,
            message,
        );
        error.range_size = Some(size);
        error
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

    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, ErrorCode::Forbidden, message)
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, ErrorCode::InvalidRequest, message)
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            message,
        )
    }
    /// The `file_conflict` error of an edit whose files changed, with the
    /// current revision and the files.
    fn file_conflict(
        current: SourceRevision,
        files: Vec<FileMismatch>,
        total: usize,
        message: String,
    ) -> Self {
        let mut conflict = Self::new(StatusCode::CONFLICT, ErrorCode::FileConflict, message);
        conflict.body.current_revision = Some(current.0);
        conflict.body.conflicting_files = Some(
            files
                .into_iter()
                .map(|file| ConflictingFile {
                    path: file.path.to_string(),
                    current_sha256: file.current.map(|digest| hex(&digest)),
                })
                .collect(),
        );
        conflict.body.conflicting_files_total = Some(total as u64);
        conflict
    }

    /// An `access_conflict` that names the access version the artifact is at.
    fn access_conflict(current: u64, message: String) -> Self {
        let mut conflict = Self::new(StatusCode::CONFLICT, ErrorCode::AccessConflict, message);
        conflict.body.current_access_version = Some(current);
        conflict
    }

    /// A `type_requirements` refusal that names the `missing` paths (decision
    /// 0059 rules 5 and 6).
    fn missing_paths(missing: &[PackagePath], message: String) -> Self {
        let mut refused = Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::TypeRequirements,
            message,
        );
        refused.body.missing_paths = Some(missing.iter().map(ToString::to_string).collect());
        refused
    }
}

impl From<ArtifactError> for ApiError {
    fn from(error: ArtifactError) -> Self {
        let message = error.to_string();
        match error {
            ArtifactError::NotFound(_) => {
                Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
            }
            // One body for every role refusal (decision 0060 rule 4).
            ArtifactError::Forbidden => Self::forbidden(message),
            ArtifactError::RevisionConflict { current } => {
                let mut conflict =
                    Self::new(StatusCode::CONFLICT, ErrorCode::RevisionConflict, message);
                conflict.body.current_revision = Some(current.0);
                conflict
            }
            ArtifactError::FilesChanged {
                current,
                files,
                total,
            } => Self::file_conflict(current, files, total, message),
            ArtifactError::MetadataConflict { current } => {
                let mut conflict =
                    Self::new(StatusCode::CONFLICT, ErrorCode::MetadataConflict, message);
                conflict.body.current_metadata_revision = Some(current.0);
                conflict
            }
            ArtifactError::AccessConflict { current } => Self::access_conflict(current.0, message),
            ArtifactError::NotInTrash => {
                Self::new(StatusCode::CONFLICT, ErrorCode::NotInTrash, message)
            }
            ArtifactError::ContentChanged => {
                Self::new(StatusCode::CONFLICT, ErrorCode::ContentConflict, message)
            }
            ArtifactError::JobEnded { .. } => {
                Self::new(StatusCode::CONFLICT, ErrorCode::JobEnded, message)
            }
            ArtifactError::TooManyJobs => {
                Self::new(StatusCode::CONFLICT, ErrorCode::TooManyJobs, message)
            }
            ArtifactError::ProfileUnknown => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::ProfileUnknown,
                message,
            ),
            ArtifactError::TypeUnknown => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::TypeUnknown,
                message,
            ),
            ArtifactError::TypeRequirements { missing } => Self::missing_paths(&missing, message),
            ArtifactError::TypeReleaseUnknown => {
                Self::new(StatusCode::CONFLICT, ErrorCode::TypeReleaseUnknown, message)
            }
            ArtifactError::OutboxBindingChanged => Self::new(
                StatusCode::CONFLICT,
                ErrorCode::OutboxBindingChanged,
                message,
            ),
            ArtifactError::DataConflict { current } => {
                let mut conflict =
                    Self::new(StatusCode::CONFLICT, ErrorCode::DataConflict, message);
                conflict.body.current_data_version = Some(current.0);
                conflict
            }
            ArtifactError::Data(DataError::PathConflict | DataError::NotObject) => {
                Self::new(StatusCode::CONFLICT, ErrorCode::DataPathConflict, message)
            }
            ArtifactError::Data(DataError::TooLarge(_)) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::DataTooLarge,
                message,
            ),
            ArtifactError::InvalidPackage(_) | ArtifactError::Data(_) => {
                Self::unprocessable(message)
            }
            ArtifactError::InvalidPageToken => Self::invalid(format!("page_token: {message}")),
            ArtifactError::InvalidRange(RangeError::PastEnd { size }) => {
                Self::unsatisfiable(size, message)
            }
            // One body for an unregistered, a disabled and the owner's id.
            ArtifactError::InvalidRange(_) | ArtifactError::NotGrantable => Self::invalid(message),
            ArtifactError::AssetLimitExceeded => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::AssetLimitExceeded,
                message,
            ),
            ArtifactError::RequestKeyReused => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::RequestKeyReused,
                message,
            ),
            ArtifactError::Unavailable(_) | ArtifactError::EditUnverified => Self::new(
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

/// The contract error of a refused asset command. Any other storage failure
/// is unavailable, and its message says to retry with the same key.
impl From<AssetStoreError> for ApiError {
    fn from(error: AssetStoreError) -> Self {
        let message = error.to_string();
        match error {
            AssetStoreError::NotFound => {
                Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
            }
            AssetStoreError::RequestKeyReused => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorCode::RequestKeyReused,
                message,
            ),
            AssetStoreError::Conflict { current } => {
                let mut conflict =
                    Self::new(StatusCode::CONFLICT, ErrorCode::AssetConflict, message);
                conflict.body.current_asset_version = Some(current.0);
                conflict
            }
            AssetStoreError::SourceConflict { current } => {
                let mut conflict =
                    Self::new(StatusCode::CONFLICT, ErrorCode::RevisionConflict, message);
                conflict.body.current_revision = Some(current.0);
                conflict
            }
            AssetStoreError::Rejected(reason) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                match reason {
                    AssetRejection::UnsupportedType => ErrorCode::AssetTypeUnsupported,
                    AssetRejection::LimitExceeded => ErrorCode::AssetLimitExceeded,
                    _ => ErrorCode::InvalidAsset,
                },
                message,
            ),
            _ => Self::unavailable(
                "asset storage unavailable; retry an identical mutation with the same key",
            ),
        }
    }
}

/// One file of a pinned revision, or the part of it a range selected.
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
        access,
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
        access: access.map(|access| match access {
            WireListAccess::Owned => ListAccess::Owned,
            WireListAccess::Shared => ListAccess::Shared,
        }),
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
        artifacts: page.artifacts.iter().map(listed_dto).collect(),
        next_page_token: page.next_page_token,
    })
}

pub(crate) async fn show(state: &AppState, actor: ActorId, id: &str) -> Result<Artifact, ApiError> {
    let id = artifact_id(id)?;
    let (record, access) = state
        .run(move |artifacts| artifacts.artifact_with_access(&actor, &id))
        .await?;
    Ok(Artifact {
        role: Some(caller_role(access)),
        ..artifact_dto(&record)
    })
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
    let artifact_type = request
        .artifact_type
        .as_deref()
        .map(|id| {
            // A malformed id names no type, as for `get_type` (rule 5).
            id.parse::<TypeId>()
                .map_err(|_| ApiError::from(ArtifactError::TypeUnknown))
        })
        .transpose()?;
    // With a type the files may be left out, and revision 1 is the starter
    // (decision 0059 rule 5); without one they are required, as before.
    let receipt = if let Some(type_id) = artifact_type {
        let files = request.files.map(upload_files).transpose()?;
        state
            .run(move |artifacts| {
                artifacts.create_from_type(&actor, &key, &title, &type_id, files.as_ref())
            })
            .await?
    } else {
        let uploads = request
            .files
            .ok_or_else(|| ApiError::invalid("files: required unless type is given"))?;
        let package = upload_package(uploads)?;
        state
            .run(move |artifacts| artifacts.create(&actor, &key, &title, &package))
            .await?
    };
    Ok(announce(state, receipt))
}

/// The package of a create's uploaded files.
fn upload_package(uploads: Vec<FileUpload>) -> Result<FilePackage, ApiError> {
    upload_files(uploads)?
        .into_package()
        .map_err(|error| ApiError::unprocessable(format!("files: {error}")))
}

/// The uploaded files of a typed create, which may lack `index.html`: the
/// type's required paths are checked first (decision 0059 rule 5).
fn upload_files(uploads: Vec<FileUpload>) -> Result<FileSet, ApiError> {
    too_many("files", uploads.len(), MAX_FILES)?;
    let mut files = Vec::with_capacity(uploads.len());
    for (
        index,
        FileUpload {
            path,
            encoding,
            content,
        },
    ) in uploads.into_iter().enumerate()
    {
        let path = package_path(&format!("files[{index}].path"), &path)?;
        files.push((
            path,
            decode(&format!("files[{index}].content"), encoding, content)?,
        ));
    }
    FileSet::new(files).map_err(|error| ApiError::unprocessable(format!("files: {error}")))
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

pub(crate) async fn edit_files(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    request: EditFilesRequest,
) -> Result<CommitResponse, ApiError> {
    let id = artifact_id(id)?;
    let base = SourceRevision(request.base_revision);
    too_many("edits", request.edits.len(), MAX_CHANGES)?;
    let mut edits = Vec::with_capacity(request.edits.len());
    for (index, edit) in request.edits.into_iter().enumerate() {
        let field = format!("edits[{index}]");
        edits.push(match edit {
            WireEdit::Put {
                path,
                encoding,
                content,
                expect,
            } => FileEdit {
                expect: expectation(&format!("{field}.expect"), &expect, true)?,
                change: FileChange::Put {
                    path: package_path(&format!("{field}.path"), &path)?,
                    content: decode(&format!("{field}.content"), encoding, content)?,
                },
            },
            WireEdit::Delete { path, expect } => FileEdit {
                expect: expectation(&format!("{field}.expect"), &expect, false)?,
                change: FileChange::Delete {
                    path: package_path(&format!("{field}.path"), &path)?,
                },
            },
        });
    }
    let outcome = state
        .run(move |artifacts| artifacts.edit_files(&actor, &key, &id, base, &edits))
        .await;
    announce_edit(state.feed(), outcome)
}

/// The answer to an edit. An edit that committed but could not be verified
/// is still an error, yet its revision exists: the change streams are woken
/// for it as for any commit.
fn announce_edit(
    feed: &Feed,
    outcome: Result<CommitReceipt, ArtifactError>,
) -> Result<CommitResponse, ApiError> {
    match outcome {
        Ok(receipt) => {
            if receipt.delivery == Delivery::Committed {
                feed.committed();
            }
            Ok(commit_response(receipt))
        }
        Err(error) => {
            if error.committed_a_revision() {
                feed.committed();
            }
            Err(error.into())
        }
    }
}

/// The condition an edit states: `absent` where `may_be_absent`, otherwise
/// a digest.
fn expectation(field: &str, value: &str, may_be_absent: bool) -> Result<FileExpectation, ApiError> {
    if value == "absent" {
        if may_be_absent {
            return Ok(FileExpectation::Absent);
        }
        return Err(ApiError::unprocessable(format!(
            "{field}: deleting a file that is not there is not an edit; give the file's sha256"
        )));
    }
    parse_sha256(value)
        .map(FileExpectation::Sha256)
        .ok_or_else(|| {
            ApiError::unprocessable(format!(
                "{field}: must be {}64 lowercase hexadecimal digits",
                if may_be_absent { "\"absent\" or " } else { "" }
            ))
        })
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
    let parts = CopyParts {
        data: request.copy_data.unwrap_or(false),
        assets: request.copy_assets.unwrap_or(false),
    };
    let receipt = state
        .run(move |artifacts| {
            artifacts.duplicate(&actor, &key, &id, revision, title.as_ref(), parts)
        })
        .await?;
    let CommitResponse {
        artifact,
        revision,
        replayed,
    } = announce(state, receipt);
    Ok(DuplicateArtifactResponse {
        artifact,
        revision,
        copied: [
            Some(CopiedPart::Files),
            parts.data.then_some(CopiedPart::Data),
            parts.assets.then_some(CopiedPart::Assets),
        ]
        .into_iter()
        .flatten()
        .collect(),
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
    // Decision 0060 rule 15: a move to the trash closes what every member
    // holds of the artifact. The store names them in the committing
    // transaction; a replay names none, so it closes nothing.
    if !receipt.trashed_members.is_empty() {
        state.revoke_members(&receipt.trashed_members, &receipt.artifact.id);
    }
    Ok(UpdateArtifactResponse {
        artifact: artifact_dto(&receipt.artifact),
        replayed: receipt.delivery == Delivery::Replayed,
    })
}

pub(crate) async fn purge(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    request: PurgeArtifactRequest,
) -> Result<PurgeArtifactResponse, ApiError> {
    let id = artifact_id(id)?;
    let base = MetadataRevision(request.base_metadata_revision);
    let receipt = state
        .run(move |artifacts| artifacts.purge(&actor, &key, &id, base))
        .await?;
    if receipt.delivery == Delivery::Committed {
        state.feed().committed();
    }
    Ok(PurgeArtifactResponse {
        artifact_id: receipt.artifact.to_string(),
        metadata_revision: receipt.metadata_revision.0,
        purged_at_ms: receipt.purged_at.0,
        replayed: receipt.delivery == Delivery::Replayed,
    })
}

/// What a cleanup of unreferenced content would remove: the dry run.
pub(crate) async fn unreferenced_content(state: &AppState) -> Result<WireContent, ApiError> {
    let content = state.run(ArtifactService::unreferenced_content).await?;
    Ok(content_dto(content))
}

/// Removes the unreferenced content that the dry run reported (`request`
/// names it), once per key.
pub(crate) async fn remove_unreferenced_content(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    request: RemoveUnreferencedContentRequest,
) -> Result<RemoveUnreferencedContentResponse, ApiError> {
    // Exactly 64 lowercase hexadecimal digits, as the dry run writes it.
    let base = ContentSetDigest::parse(&request.base_digest)
        .ok_or_else(|| ApiError::invalid("base_digest must be 64 lowercase hexadecimal digits"))?;
    let receipt = state
        .run(move |artifacts| artifacts.remove_unreferenced_content(&actor, &key, base))
        .await?;
    Ok(RemoveUnreferencedContentResponse {
        blobs: receipt.blobs,
        bytes: receipt.bytes,
        replayed: receipt.delivery == Delivery::Replayed,
    })
}

fn content_dto(content: UnreferencedContent) -> WireContent {
    WireContent {
        blobs: content.blobs,
        bytes: content.bytes,
        digest: content.digest.to_string(),
    }
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

/// Reads the part of a file that `range` selects, cut at `boundary`
/// (decision 0016); [`ByteRange::WHOLE`] reads all of it.
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
    let runtime = if export.files.files().any(|(path, content)| {
        path.as_str() == ENTRY_PATH && preview::runtime_marker(content).is_some()
    }) {
        ExportRuntime::Metadata
    } else {
        ExportRuntime::None
    };
    let manifest = SourceExportManifest {
        format: if runtime == ExportRuntime::Metadata {
            SourceExportFormat::V2
        } else {
            SourceExportFormat::V1
        },
        artifact_id: export.artifact.id.to_string(),
        title: export.artifact.title.as_str().to_owned(),
        revision: revision.revision,
        created_at_ms: revision.created_at_ms,
        entry: ENTRY_PATH.to_owned(),
        runtime,
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

/// The management preview operation: a link to `revision`'s own files, or to
/// its build of a profile (decision 0058 rule 14). A revision without that
/// build is refused here, before any ticket exists.
pub(crate) async fn preview(
    state: &AppState,
    actor: ActorId,
    token_id: Option<TokenId>,
    generation: StateGeneration,
    id: &str,
    revision: u64,
    target: PreviewTarget,
) -> Result<Preview, ApiError> {
    let id = artifact_id(id)?;
    let revision = revision_number(revision)?;
    let lookup_actor = actor.clone();
    let lookup_id = id.clone();
    let lookup_target = target.clone();
    let build_profiles = match &target {
        PreviewTarget::Source => Vec::new(),
        PreviewTarget::Build(_) => state.settings().build_profiles.clone(),
    };
    let seen = state
        .run(move |artifacts| {
            // Read before the access check (decision 0060 rule 16).
            let seen = artifacts.access_generation(&lookup_actor, &lookup_id)?;
            match &lookup_target {
                PreviewTarget::Source => {
                    let record = artifacts.artifact(&lookup_actor, &lookup_id)?;
                    if revision > record.current_revision {
                        return Err(ArtifactError::NotFound(Missing::Revision));
                    }
                }
                PreviewTarget::Build(profile) => {
                    artifacts.preview_build(
                        &lookup_actor,
                        &lookup_id,
                        revision,
                        profile,
                        &build_profiles,
                    )?;
                }
            }
            Ok(seen)
        })
        .await?;
    let owner = actor.clone();
    let named = id.clone();
    let route = target.route();
    let ticket = state
        .previews()
        .issue(actor, token_id, id, revision, target);
    let ticket = ticket.ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "too many live previews; retry shortly",
        )
    })?;
    // The link is a bearer capability that outlives the request: if the user
    // was disabled, or disabled and enabled again, while this request ran, it
    // is removed again, and so it is if the user's access to the artifact
    // changed.
    state.ensure_current(&owner, generation).await?;
    state.confirm_access(&owner, &named, seen).await?;
    Ok(Preview {
        url: format!(
            "{}/{route}/{}/",
            state.settings().content_origin,
            ticket.secret
        ),
        revision: revision.0,
        expires_at_ms: ticket.expires_at_ms,
    })
}

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub(crate) fn validate_view(view_id: &str, generation: u64) -> Result<(), ApiError> {
    if view_id.len() != 32
        || !view_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !(1..=MAX_SAFE_INTEGER).contains(&generation)
    {
        return Err(ApiError::invalid("invalid runtime view or generation"));
    }
    Ok(())
}

pub(crate) fn missing_view() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        ErrorCode::NotFound,
        "runtime view not found",
    )
}

pub(crate) fn session_error(error: SessionError) -> ApiError {
    match error {
        SessionError::Missing | SessionError::Closed => ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::RuntimeSessionExpired,
            "runtime connection is closed or expired",
        ),
        SessionError::Conflict => ApiError::new(
            StatusCode::CONFLICT,
            ErrorCode::InvalidRequest,
            "runtime grant changed; refresh before trying again",
        ),
        SessionError::Unavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "runtime connections are temporarily unavailable",
        ),
    }
}

fn handoff_error(error: HandoffError) -> ApiError {
    match error {
        HandoffError::Missing => ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "handoff or agent session not found",
        ),
        HandoffError::Invalid(message) => ApiError::unprocessable(message),
        HandoffError::Conflict(message) => {
            ApiError::new(StatusCode::CONFLICT, ErrorCode::InvalidRequest, message)
        }
        HandoffError::Unavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "handoffs are temporarily unavailable",
        ),
        HandoffError::ConnectionClosed => session_error(SessionError::Closed),
    }
}

/// Registers an agent inbox. It only registers: a request that serves a
/// caller uses [`register_confirmed_handoff_session`], which also confirms
/// that the user is still enabled.
pub(crate) fn register_handoff_session(
    state: &AppState,
    actor: &ActorId,
    name: &str,
) -> Result<RegisteredHandoffSession, ApiError> {
    state
        .handoffs()
        .register(actor, name)
        .map_err(handoff_error)
}

/// Registers an agent inbox for a request that authenticated earlier. The
/// inbox outlives the request, so the user is confirmed **after** it is
/// registered: a disable that came in between (and an enable after it) has
/// already run its cleanup, and this removes the inbox again (decision 0036).
pub(crate) async fn register_confirmed_handoff_session(
    state: &AppState,
    actor: &ActorId,
    generation: StateGeneration,
    name: &str,
) -> Result<RegisteredHandoffSession, ApiError> {
    let registered = register_handoff_session(state, actor, name)?;
    state.ensure_current(actor, generation).await?;
    Ok(registered)
}

pub(crate) fn agent_sessions(
    state: &AppState,
    actor: &ActorId,
    workspace_id: &str,
) -> Result<AgentSessionList, ApiError> {
    validate_view(workspace_id, 1)?;
    let workspace = state
        .runtime_sessions()
        .get(actor, workspace_id)
        .map_err(session_error)?;
    let sessions = workspace
        .while_active(|| state.handoffs().list(actor))
        .map_err(session_error)?
        .map_err(handoff_error)?;
    Ok(AgentSessionList { sessions })
}

pub(crate) async fn stage_handoff(
    state: &AppState,
    actor: ActorId,
    request: StageHandoffRequest,
) -> Result<Handoff, ApiError> {
    validate_view(&request.workspace_session_id, 1)?;
    validate_view(&request.agent_session_id, 1)?;
    let workspace = state
        .runtime_sessions()
        .get(&actor, &request.workspace_session_id)
        .map_err(session_error)?;
    let intent = handoffs::intent(&request.intent).map_err(handoff_error)?;
    let named = artifact_id(&request.artifact_id)?;
    // Read before `read_file`'s access check (decision 0060 rule 16).
    let seen = state.access_seen(&actor, &named).await?;
    let file = read_file(
        state,
        actor.clone(),
        &request.artifact_id,
        request.revision,
        &request.path,
        ByteRange::WHOLE,
        Boundary::Byte,
    )
    .await?;
    let selection = handoffs::selection(
        request.artifact_id,
        request.revision,
        file.path.to_string(),
        &request.file_sha256,
        request.start,
        request.end,
        file.part,
    )
    .map_err(handoff_error)?;
    let staged = state
        .handoffs()
        .stage(
            &actor,
            &request.workspace_session_id,
            &workspace,
            &request.agent_session_id,
            selection,
            intent,
        )
        .map_err(handoff_error)?;
    state.confirm_access(&actor, &named, seen).await?;
    Ok(staged)
}

pub(crate) fn get_handoff(
    state: &AppState,
    actor: &ActorId,
    workspace_id: &str,
    id: &str,
) -> Result<Handoff, ApiError> {
    validate_view(workspace_id, 1)?;
    validate_view(id, 1)?;
    state
        .handoffs()
        .get(actor, workspace_id, id)
        .map_err(handoff_error)
}

pub(crate) async fn send_handoff(
    state: &AppState,
    actor: ActorId,
    workspace_id: &str,
    id: &str,
) -> Result<Handoff, ApiError> {
    let handoff = get_handoff(state, &actor, workspace_id, id)?;
    if handoff_was_sent(&handoff)? {
        return Ok(handoff);
    }
    if let Err(error) = confirm_handoff_source(state, actor.clone(), &handoff).await {
        // A concurrent send may have committed while this lookup ran.
        let current = get_handoff(state, &actor, workspace_id, id)?;
        if handoff_was_sent(&current)? {
            return Ok(current);
        }
        return Err(error);
    }
    match state.handoffs().send(&actor, workspace_id, id) {
        Ok(sent) => Ok(sent),
        Err(error) => {
            // Closing a connection or inbox after a concurrent send must not
            // hide an outcome that is already readable by this workspace.
            let current = get_handoff(state, &actor, workspace_id, id)?;
            if handoff_was_sent(&current)? {
                Ok(current)
            } else {
                Err(handoff_error(error))
            }
        }
    }
}

fn handoff_was_sent(handoff: &Handoff) -> Result<bool, ApiError> {
    match handoff.state {
        HandoffState::Staged => Ok(false),
        HandoffState::Queued
        | HandoffState::Offered
        | HandoffState::Blocked
        | HandoffState::Acknowledged => Ok(true),
        HandoffState::Cancelled | HandoffState::Expired => Err(handoff_error(
            HandoffError::Conflict("this handoff cannot be sent"),
        )),
    }
}

pub(crate) fn cancel_handoff(
    state: &AppState,
    actor: &ActorId,
    workspace_id: &str,
    id: &str,
) -> Result<Handoff, ApiError> {
    validate_view(workspace_id, 1)?;
    validate_view(id, 1)?;
    state
        .handoffs()
        .cancel(actor, workspace_id, id)
        .map_err(handoff_error)
}

pub(crate) async fn poll_handoffs(
    state: &AppState,
    actor: ActorId,
    session_id: &str,
    secret: &str,
) -> Result<HandoffInbox, ApiError> {
    validate_view(session_id, 1)?;
    let mut deliveries = state
        .handoffs()
        .poll(&actor, session_id, secret)
        .map_err(handoff_error)?;
    if let Some(delivery) = deliveries.first() {
        // Once offered it cannot be recalled, even if this response fails.
        if let Err(error) = confirm_handoff_source(state, actor.clone(), delivery).await {
            if matches!(error.status, StatusCode::NOT_FOUND | StatusCode::CONFLICT) {
                state
                    .handoffs()
                    .block_offer(&actor, session_id, secret, &delivery.id)
                    .map_err(handoff_error)?;
            }
            return Err(error);
        }
        if !state
            .handoffs()
            .confirm_offer(&actor, session_id, secret, &delivery.id)
            .map_err(handoff_error)?
        {
            deliveries.clear();
        }
    }
    Ok(HandoffInbox { deliveries })
}

async fn confirm_handoff_source(
    state: &AppState,
    actor: ActorId,
    handoff: &Handoff,
) -> Result<(), ApiError> {
    let selected = &handoff.selection;
    let file = read_file(
        state,
        actor,
        &selected.artifact_id,
        selected.revision,
        &selected.path,
        ByteRange::WHOLE,
        Boundary::Byte,
    )
    .await?;
    if hex(&file.part.sha256) != selected.file_sha256 {
        return Err(handoff_error(HandoffError::Conflict(
            "the saved file no longer matches the selection",
        )));
    }
    Ok(())
}

pub(crate) fn acknowledge_handoff(
    state: &AppState,
    actor: &ActorId,
    session_id: &str,
    secret: &str,
    id: &str,
) -> Result<HandoffReceipt, ApiError> {
    validate_view(session_id, 1)?;
    validate_view(id, 1)?;
    state
        .handoffs()
        .acknowledge(actor, session_id, secret, id)
        .map_err(handoff_error)
}

/// Opens a connection. It only registers: a request that serves a caller
/// uses [`open_confirmed_runtime_session`], which also confirms that the user
/// is still enabled.
pub(crate) fn open_runtime_session(
    state: &AppState,
    actor: &ActorId,
    token_id: Option<TokenId>,
) -> Result<RuntimeConnection, ApiError> {
    let session_id = state
        .runtime_sessions()
        .create(actor, token_id)
        .map_err(session_error)?;
    Ok(RuntimeConnection { session_id })
}

/// Opens a connection for a request that authenticated earlier. The
/// connection outlives the request, so the user is confirmed **after** it is
/// registered: a disable that came in between (and an enable after it) has
/// already run its cleanup, and this closes and forgets the connection again
/// (decision 0036).
pub(crate) async fn open_confirmed_runtime_session(
    state: &AppState,
    actor: &ActorId,
    token_id: Option<TokenId>,
    generation: StateGeneration,
) -> Result<RuntimeConnection, ApiError> {
    let opened = open_runtime_session(state, actor, token_id)?;
    state.ensure_current(actor, generation).await?;
    Ok(opened)
}

pub(crate) fn close_runtime_session(
    state: &AppState,
    actor: &ActorId,
    id: &str,
) -> Result<(), ApiError> {
    validate_view(id, 1)?;
    state
        .runtime_sessions()
        .close(actor, id)
        .map_err(session_error)?;
    // The connection is closed first, so no join can pass its check after
    // this; what it held in a room goes now rather than when its lease ends.
    state.rooms().leave_tab(actor, &TabKey::new(id));
    Ok(())
}

/// What a runtime request reads of the revision and of the caller's access.
struct RuntimeSource {
    record: ArtifactRecord,
    /// Whether the entry opts in to the runtime.
    enabled: bool,
    policy: RuntimePolicy,
    /// The access generation read before the access check (decision 0060
    /// rule 16).
    seen: AccessGeneration,
    /// The capabilities the caller's role offers (decision 0060 rule 3).
    offered: CapabilitySet,
}

async fn runtime_source(
    state: &AppState,
    actor: &ActorId,
    id: &ArtifactId,
    revision: SourceRevision,
) -> Result<RuntimeSource, ApiError> {
    let actor = actor.clone();
    let id = id.clone();
    let manifest_path: PackagePath = MANIFEST_PATH.parse().map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "invalid runtime manifest path",
        )
    })?;
    let (record, enabled, manifest, seen, may_change) = state
        .run(move |artifacts| {
            // Read before the access check (decision 0060 rule 16).
            let seen = artifacts.access_generation(&actor, &id)?;
            let record = artifacts.artifact(&actor, &id)?;
            let may_change = artifacts.allows(&actor, &id, Need::Change)?;
            if revision > record.current_revision {
                return Err(ArtifactError::NotFound(Missing::Revision));
            }
            let entry = artifacts.read_file(&actor, &id, revision, &PackagePath::entry())?;
            let enabled = preview::runtime_marker(&entry).is_some();
            let manifest = if enabled {
                match artifacts.read_file(&actor, &id, revision, &manifest_path) {
                    Ok(bytes) => Some(bytes),
                    Err(ArtifactError::NotFound(Missing::File)) => None,
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
            Ok((record, enabled, manifest, seen, may_change))
        })
        .await?;
    let policy = RuntimePolicy::from_source(enabled, manifest.as_deref())
        .map_err(ApiError::unprocessable)?;
    Ok(RuntimeSource {
        record,
        enabled,
        policy,
        seen,
        offered: CapabilitySet::offered(may_change),
    })
}

pub(crate) async fn set_runtime_grant(
    state: &AppState,
    actor: ActorId,
    session_id: &str,
    id: &str,
    revision: u64,
    request: SetRuntimeGrantRequest,
) -> Result<RuntimeGrant, ApiError> {
    validate_view(session_id, 1)?;
    let session = state
        .runtime_sessions()
        .get(&actor, session_id)
        .map_err(session_error)?;
    let id = artifact_id(id)?;
    let revision = revision_number(revision)?;
    let RuntimeSource {
        record,
        policy,
        seen,
        offered,
        ..
    } = runtime_source(state, &actor, &id, revision).await?;
    if record.current_revision.0 != request.head_revision {
        return Err(session_error(SessionError::Conflict));
    }
    let Some(capabilities) = policy.grant_for(&request.capabilities, offered) else {
        return Err(ApiError::unprocessable(
            "grant must be a supported, declared subset",
        ));
    };
    let scope = GrantScope::new(id.clone(), revision, record.current_revision);
    let version = session
        .set_grant(scope, capabilities, request.expected_version)
        .map_err(session_error)?;
    // A grant that holds a capability outlives the request, so it is
    // confirmed against the access the request saw (decision 0060 rule 16).
    // A withdrawal grants nothing to confirm and takes effect at once.
    if capabilities != CapabilitySet::default() {
        state.confirm_access(&actor, &id, seen).await?;
    }
    Ok(RuntimeGrant { version })
}

fn not_adoptable() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        ErrorCode::InvalidRequest,
        "the runtime view cannot be adopted",
    )
}

fn runtime_issue_error(error: RuntimeIssueError) -> ApiError {
    match error {
        RuntimeIssueError::Stale => ApiError::new(
            StatusCode::CONFLICT,
            ErrorCode::InvalidRequest,
            "a newer runtime view already exists",
        ),
        RuntimeIssueError::Unavailable => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "runtime views are temporarily unavailable",
        ),
        RuntimeIssueError::NotAdoptable => not_adoptable(),
    }
}

/// [`runtime_source`] for a runtime view. Reading a revision answers 422 when
/// its manifest is unusable (another SDK pin, a malformed field); an adoption
/// of such a revision is refused, not malformed (decision 0054, rule 16).
async fn view_source(
    state: &AppState,
    actor: &ActorId,
    id: &ArtifactId,
    revision: SourceRevision,
    adopting: bool,
) -> Result<RuntimeSource, ApiError> {
    match runtime_source(state, actor, id, revision).await {
        Err(error) if adopting && error.status == StatusCode::UNPROCESSABLE_ENTITY => {
            Err(not_adoptable())
        }
        other => other,
    }
}

pub(crate) async fn create_runtime_view(
    state: &AppState,
    actor: ActorId,
    token_id: Option<TokenId>,
    generation: StateGeneration,
    id: &str,
    revision: u64,
    request: CreateRuntimeViewRequest,
) -> Result<RuntimeView, ApiError> {
    validate_view(&request.view_id, request.generation)?;
    validate_view(&request.session_id, 1)?;
    if let Some(adopt) = request.adopt {
        validate_view(&request.view_id, adopt.generation)?;
    }
    let session = state
        .runtime_sessions()
        .get(&actor, &request.session_id)
        .map_err(session_error)?;
    let id = artifact_id(id)?;
    let revision = revision_number(revision)?;
    let _creation = state
        .previews()
        .begin_runtime(&actor, &request.view_id)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                "runtime views are temporarily unavailable",
            )
        })?;
    let source = view_source(state, &actor, &id, revision, request.adopt.is_some()).await?;
    let RuntimeSource {
        record,
        enabled,
        policy,
        seen,
        offered,
    } = source;
    let scope = GrantScope::new(id.clone(), revision, record.current_revision);
    let owner = actor.clone();
    let lifecycle = policy.lifecycle();
    if request.adopt.is_some() {
        // An adoption changes a live view, so a request that cannot be
        // confirmed is refused before it changes one; the confirmation after
        // the registration stays, as for a new view.
        state.ensure_current(&owner, generation).await?;
    }
    let adoption = request.adopt.map(|adopt| preview::Adoption {
        generation: adopt.generation,
        session: session.clone(),
    });
    let (ticket, grant_version, granted) = session
        .with_grant(&scope, |version, granted| {
            let ticket = state.previews().issue_runtime(RuntimeTicketRequest {
                actor,
                token_id,
                artifact: id.clone(),
                revision,
                head_revision: record.current_revision,
                view_id: request.view_id.clone(),
                generation: request.generation,
                lifecycle,
                adopt: adoption,
            })?;
            let authority = RuntimeAuthority {
                session: session.clone(),
                scope: scope.clone(),
                version,
            };
            if !state
                .previews()
                .bind_authority(&ticket.preview.secret, authority)
            {
                return Err(RuntimeIssueError::Unavailable);
            }
            Ok((ticket, version, granted))
        })
        .map_err(session_error)?
        .map_err(runtime_issue_error)?;
    // As for a preview link: confirmed after the registration (see `preview`).
    state.ensure_current(&owner, generation).await?;
    state.confirm_access(&owner, &id, seen).await?;
    Ok(RuntimeView {
        url: format!(
            "{}/{}/{}/",
            state.settings().content_origin,
            if enabled { "r" } else { "p" },
            ticket.preview.secret
        ),
        enabled,
        lifecycle: lifecycle.into(),
        session_id: request.session_id,
        artifact_id: id.to_string(),
        revision: revision.0,
        head_revision: record.current_revision.0,
        sdk: enabled.then(|| SDK_PIN.to_owned()),
        declared: policy.declared(),
        supported: if enabled { offered.names() } else { Vec::new() },
        // A grant held from a role that offered more (until S02e removes it)
        // is reported as the role offers it now (decision 0060 rule 3).
        granted: granted.and(offered).names(),
        grant_version,
        view_id: request.view_id,
        generation: request.generation,
        initialization: ticket.initialization,
        expires_at_ms: ticket.preview.expires_at_ms,
        lease_ms: ticket.lease_ms,
    })
}

pub(crate) async fn runtime_metadata(
    state: &AppState,
    actor: ActorId,
    id: &str,
    revision: u64,
    view_id: &str,
    generation: u64,
    initialization: &str,
) -> Result<RuntimeMetadata, ApiError> {
    validate_view(view_id, generation)?;
    let id = artifact_id(id)?;
    let revision = revision_number(revision)?;
    let key = ViewKey {
        actor: &actor,
        artifact: &id,
        revision,
        view_id,
        generation,
        initialization,
    };
    // Any refusal is the same `404`: this route predates the capability
    // distinction (decisions 0019 and 0020) and keeps its answers.
    let head = state
        .previews()
        .runtime_access(&key, Capability::MetadataRead)
        .map_err(|_| missing_view())?;
    let lookup_actor = actor.clone();
    let lookup_id = id.clone();
    let record = state
        .run(move |artifacts| artifacts.artifact(&lookup_actor, &lookup_id))
        .await?;
    // Access and current source are read from the service on every RPC. A
    // concurrent view replacement or expiry is checked again after the read.
    if record.current_revision != head
        || state
            .previews()
            .runtime_access(&key, Capability::MetadataRead)
            != Ok(head)
    {
        return Err(missing_view());
    }
    Ok(RuntimeMetadata {
        artifact_id: record.id.to_string(),
        title: record.title.as_str().to_owned(),
        current_revision: record.current_revision.0,
        metadata_revision: record.metadata_revision.0,
    })
}

pub(crate) fn close_runtime_view(
    state: &AppState,
    actor: &ActorId,
    view_id: &str,
    generation: u64,
) -> Result<(), ApiError> {
    validate_view(view_id, generation)?;
    state
        .previews()
        .close_runtime(actor, view_id, generation)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                "runtime views are temporarily unavailable",
            )
        })?;
    Ok(())
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

/// Parses a job identifier. One that cannot name a job is not found, as for
/// an artifact, and is not quoted back.
pub(crate) fn job_id(value: &str) -> Result<JobId, ApiError> {
    value
        .parse()
        .map_err(|_| ApiError::not_found("job not found"))
}

/// Parses an asset identifier. One that cannot name an asset is not found, as
/// for an artifact, and is not quoted back.
pub(crate) fn asset_id(value: &str) -> Result<AssetId, ApiError> {
    value
        .parse()
        .map_err(|_| ApiError::not_found("asset not found"))
}

pub(crate) async fn enqueue_job(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    request: EnqueueJobRequest,
) -> Result<EnqueueJobResponse, ApiError> {
    let id = artifact_id(id)?;
    let revision = revision_number(request.revision)?;
    let profile = match (request.kind, request.profile) {
        (WireJobKind::VerifyRevision, None) => None,
        (WireJobKind::VerifyRevision, Some(_)) => {
            return Err(ApiError::invalid(
                "profile: only a run_profile job names one",
            ));
        }
        (WireJobKind::RunProfile, None) => {
            return Err(ApiError::invalid("profile: a run_profile job requires one"));
        }
        (WireJobKind::RunProfile, Some(name)) => Some(profile_name(&name)?),
    };
    let now = state.clock().now();
    // Only a build reads the build profiles, so a verification copies none.
    let builds = if profile.is_some() {
        state.settings().build_profiles.clone()
    } else {
        Vec::new()
    };
    let (receipt, diagnostics) = state
        .run(move |artifacts| {
            let receipt = match &profile {
                None => artifacts.enqueue_job(
                    &actor,
                    &key,
                    &id,
                    revision,
                    JobKind::VerifyRevision,
                    now,
                )?,
                Some(profile) => artifacts.enqueue_build(
                    &actor,
                    &key,
                    &NewBuild {
                        artifact: &id,
                        revision,
                        profile,
                        build_profiles: &builds,
                    },
                    now,
                )?,
            };
            let diagnostics = artifacts.job_diagnostics(&receipt.job)?;
            Ok((receipt, diagnostics))
        })
        .await?;
    if receipt.delivery == Delivery::Committed {
        // The commit may append a `job_changed` row to the owner's feed
        // (decision 0055, rule 7); wake the streams as a commit does.
        state.feed().committed();
    }
    Ok(EnqueueJobResponse {
        job: job_dto(&receipt.job, &diagnostics)?,
        replayed: receipt.delivery == Delivery::Replayed,
    })
}

/// The profile a build request names (decision 0058 rules 14, 16 and 17).
/// A name that cannot be a profile is not one this build knows either: it
/// is `profile_unknown`, and it is not quoted back. The core then checks
/// that the profile is a build profile, before anything is read.
pub(crate) fn profile_name(name: &str) -> Result<ProfileName, ApiError> {
    name.parse()
        .map_err(|_| ApiError::from(ArtifactError::ProfileUnknown))
}

/// The head status of an artifact's builds of `profile` (decision 0058 rule
/// 16): only for a caller who may read the artifact outside the trash.
pub(crate) async fn build_status(
    state: &AppState,
    actor: ActorId,
    id: &str,
    profile: &str,
) -> Result<WireBuildStatus, ApiError> {
    let profile = profile_name(profile)?;
    let id = artifact_id(id)?;
    let name = profile.as_str().to_owned();
    let builds = state.settings().build_profiles.clone();
    let status = state
        .run(move |artifacts| artifacts.build_status(&actor, &id, &profile, &builds))
        .await?;
    let (head, job_id, failure, diagnostics) = match status.head_build {
        HeadBuild::Built => (HeadBuildStatus::Built, None, None, Vec::new()),
        HeadBuild::Pending { job } => (
            HeadBuildStatus::Pending,
            Some(job.to_string()),
            None,
            Vec::new(),
        ),
        HeadBuild::Failed {
            job,
            failure,
            diagnostics,
        } => (
            HeadBuildStatus::Failed,
            Some(job.to_string()),
            Some(failure_dto(failure)),
            diagnostic_dtos(&diagnostics),
        ),
        HeadBuild::None => (HeadBuildStatus::None, None, None, Vec::new()),
    };
    Ok(WireBuildStatus {
        profile: name,
        head_revision: status.head.0,
        latest_build_revision: status.latest.map(|build| build.revision.0),
        status: head,
        job_id,
        failure,
        diagnostics,
    })
}

/// What `GET /v1/background` reports: the server's own worker (decision 0055,
/// rule 5) and whether an execution worker is ready (decision 0058 rule 20),
/// read from the database that the execution workers share. A store that
/// cannot be read reports no worker ready, since no build can start then, and
/// keeps the server's own worker status, which an operator needs most while
/// storage fails.
pub(crate) async fn background(state: &AppState) -> BackgroundStatus {
    let now = state.clock().now();
    let ready = state
        .run(move |artifacts| artifacts.execution_ready(now))
        .await
        .unwrap_or(false);
    BackgroundStatus {
        worker: state.settings().worker_status.current(),
        execution: ExecutionStatus { ready },
    }
}

/// The wire form of `release`, as this server's build sees it.
fn type_summary(state: &AppState, release: &Release) -> TypeSummary {
    TypeSummary {
        id: release.id.to_string(),
        release: u64::from(release.release.get()),
        name: format!("{}-{}", release.id, release.release),
        title: release.title.clone(),
        description: release.description.clone(),
        kind: wire_kind(release.kind),
        required: release.required.iter().map(ToString::to_string).collect(),
        starter: release
            .starter
            .files()
            .map(|(path, _)| path.to_string())
            .collect(),
        build_profile: release.build_profile.as_ref().map(ToString::to_string),
        build_available: release.build_available(&state.settings().build_profiles),
        references: release
            .references
            .iter()
            .map(|(path, _)| path.to_string())
            .collect(),
    }
}

/// The current release of each catalog type (decision 0059 rule 11).
pub(crate) fn list_types(state: &AppState) -> TypeList {
    TypeList {
        notice: TYPE_NOTICE.to_owned(),
        types: state
            .catalog()
            .current_releases()
            .map(|release| type_summary(state, release))
            .collect(),
    }
}

/// The release number of a type route's path segment. A malformed one names
/// no release, so it is not found, as rule 11 asks for an unknown one.
pub(crate) fn release_segment(release: &str) -> Result<TypeRelease, ApiError> {
    release
        .parse()
        .map_err(|_| ApiError::not_found("type not found"))
}

/// The release `release` of the type `id` (its current one for `None`). An
/// id that is malformed or unknown, or a release the type does not have, is
/// not found, as rule 11 asks.
fn find_release<'a>(
    state: &'a AppState,
    id: &str,
    release: Option<TypeRelease>,
) -> Result<&'a Release, ApiError> {
    let missing = || ApiError::not_found("type not found");
    let id: TypeId = id.parse().map_err(|_| missing())?;
    let found = match release {
        None => state.catalog().current(&id),
        Some(release) => state.catalog().release(&id, release),
    };
    found.ok_or_else(missing)
}

/// One release of a type with its guide (decision 0059 rule 11).
pub(crate) fn show_type(
    state: &AppState,
    id: &str,
    release: Option<TypeRelease>,
) -> Result<TypeDescriptor, ApiError> {
    let release = find_release(state, id, release)?;
    Ok(TypeDescriptor {
        notice: TYPE_NOTICE.to_owned(),
        release: type_summary(state, release),
        guide: release.guide.clone(),
    })
}

/// One reference file of a release (decision 0059 rule 11), of the current
/// release for `None`.
pub(crate) fn type_reference(
    state: &AppState,
    id: &str,
    release: Option<TypeRelease>,
    path: &str,
) -> Result<TypeReference, ApiError> {
    let found = find_release(state, id, release)?;
    let missing = || ApiError::not_found("reference not found");
    let path: PackagePath = path.parse().map_err(|_| missing())?;
    let content = found.reference(&path).ok_or_else(missing)?;
    Ok(TypeReference {
        notice: TYPE_NOTICE.to_owned(),
        id: found.id.to_string(),
        release: u64::from(found.release.get()),
        path: path.to_string(),
        content: content.to_owned(),
    })
}

pub(crate) async fn show_job(
    state: &AppState,
    actor: ActorId,
    id: &str,
) -> Result<WireJob, ApiError> {
    let id = job_id(id)?;
    let (job, diagnostics) = state
        .run(move |artifacts| {
            let job = artifacts.job(&actor, &id)?;
            let diagnostics = artifacts.job_diagnostics(&job)?;
            Ok((job, diagnostics))
        })
        .await?;
    job_dto(&job, &diagnostics)
}

/// A page of the actor's jobs: those of the artifact `artifact` names, or all
/// of them.
pub(crate) async fn list_jobs(
    state: &AppState,
    actor: ActorId,
    artifact: Option<&str>,
    query: ListJobsQuery,
) -> Result<JobList, ApiError> {
    let artifact = artifact.map(artifact_id).transpose()?;
    let limit = query
        .limit
        .map(JobPageSize::try_from)
        .transpose()
        .map_err(|error| ApiError::invalid(format!("limit: {error}")))?
        .unwrap_or_default();
    let page_token = query.page_token;
    let (page, diagnostics) = state
        .run(move |artifacts| {
            let page = artifacts.jobs(&actor, artifact.as_ref(), page_token.as_deref(), limit)?;
            let diagnostics = page
                .jobs
                .iter()
                .map(|job| artifacts.job_diagnostics(job))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((page, diagnostics))
        })
        .await?;
    Ok(JobList {
        jobs: page
            .jobs
            .iter()
            .zip(&diagnostics)
            .map(|(job, diagnostics)| job_dto(job, diagnostics))
            .collect::<Result<Vec<_>, _>>()?,
        next_page_token: page.next_page_token,
    })
}

pub(crate) async fn cancel_job(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
) -> Result<CancelJobResponse, ApiError> {
    let id = job_id(id)?;
    let now = state.clock().now();
    let (receipt, diagnostics) = state
        .run(move |artifacts| {
            let receipt = artifacts.cancel_job(&actor, &key, &id, now)?;
            let diagnostics = artifacts.job_diagnostics(&receipt.job)?;
            Ok((receipt, diagnostics))
        })
        .await?;
    if receipt.delivery == Delivery::Committed {
        // The commit may append a `job_changed` row to the owner's feed
        // (decision 0055, rule 7); wake the streams as a commit does.
        state.feed().committed();
    }
    Ok(CancelJobResponse {
        job: job_dto(&receipt.job, &diagnostics)?,
        replayed: receipt.delivery == Delivery::Replayed,
    })
}

/// Parses a data path. A path that breaks the grammar is refused with the rule,
/// never with the path.
pub(crate) fn data_path(field: &str, value: &str) -> Result<DataPath, ApiError> {
    value
        .parse()
        .map_err(|error: DataError| ApiError::unprocessable(format!("{field}: {error}")))
}

/// Applies the rules of application data to JSON text as it was sent. A
/// frame's value arrives as text (decision 0053, rule 5). Text over the value
/// limit is refused as `data_too_large`; any other refusal is unprocessable.
pub(crate) fn data_text(field: &str, text: &str) -> Result<DataValue, ApiError> {
    DataValue::parse(text).map_err(|error| match error {
        DataError::TooLarge(_) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::DataTooLarge,
            format!("{field}: {error}"),
        ),
        _ => ApiError::unprocessable(format!("{field}: {error}")),
    })
}

/// Applies the rules of application data to a value as it was sent.
fn data_value(field: &str, value: &JsonValue) -> Result<DataValue, ApiError> {
    data_text(field, value.get())
}

/// The space a request names on the wire; the caller's own when absent
/// (decision 0060 rule 10).
pub(crate) fn data_space(name: Option<DataSpaceName>) -> Space {
    match name.unwrap_or_default() {
        DataSpaceName::Own => Space::Own,
        DataSpaceName::Shared => Space::Shared,
    }
}

/// The value at a path of the actor's data space of an artifact, or of its
/// shared space.
pub(crate) async fn read_data(
    state: &AppState,
    actor: ActorId,
    id: &str,
    space: Space,
    path: Option<&str>,
) -> Result<AppData, ApiError> {
    let id = artifact_id(id)?;
    let path = data_path("path", path.unwrap_or_default())?;
    let wire_path = path.to_string();
    let artifact = id.to_string();
    let read = state
        .run(move |artifacts| artifacts.data(&actor, &id, space, &path))
        .await?;
    Ok(AppData {
        artifact_id: artifact,
        path: wire_path,
        data_version: read.version.0,
        exists: read.value.is_some(),
        value: read
            .value
            .and_then(|value| JsonValue::from_text(value.to_canonical())),
    })
}

/// Most characters of a page token of a member listing: the longest token
/// in base64 without padding, which writes every 3 bytes as 4 characters and
/// the rest as 2 or 3, that is ceil(4n / 3) for n bytes (decision 0040, rule
/// 10; 107 for the 80 bytes of today's layout).
const MAX_MEMBER_TOKEN_CHARS: usize = (MAX_TOKEN_BYTES * 4).div_ceil(3);

/// Reads the page token of a member listing. A token that is too long, not
/// base64url without padding, or not the layout of rule 10 is refused with
/// the rule, never with the token. Whether it was issued for this artifact
/// and path is checked by the core.
fn member_token(text: &str) -> Result<MemberToken, ApiError> {
    let refused = || ApiError::invalid("page_token: not a token issued by this listing");
    if text.len() > MAX_MEMBER_TOKEN_CHARS {
        return Err(refused());
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| refused())?;
    MemberToken::from_bytes(&bytes).ok_or_else(refused)
}

/// One page of the member names of an object in the actor's data space.
pub(crate) async fn read_data_members(
    state: &AppState,
    actor: ActorId,
    id: &str,
    query: ReadDataMembersQuery,
) -> Result<DataMembers, ApiError> {
    let id = artifact_id(id)?;
    let ReadDataMembersQuery {
        space,
        path,
        limit,
        page_token,
        at_data_version,
    } = query;
    let limit = limit
        .map(MemberLimit::try_from)
        .transpose()
        .map_err(|error| ApiError::invalid(format!("limit: {error}")))?
        .unwrap_or_default();
    let path = data_path("path", path.as_deref().unwrap_or_default())?;
    let after = page_token.as_deref().map(member_token).transpose()?;
    let query = MemberQuery {
        space: data_space(space),
        path,
        after,
        limit,
        at_data_version: at_data_version.map(AppDataVersion),
    };
    let artifact = id.to_string();
    let wire_path = query.path.to_string();
    let page = state
        .run(move |artifacts| artifacts.data_members(&actor, &id, &query))
        .await
        .map_err(|error| match error {
            ArtifactError::InvalidPageToken => ApiError::invalid(
                "page_token: not a token issued for this artifact, space and path",
            ),
            other => other.into(),
        })?;
    Ok(DataMembers {
        artifact_id: artifact,
        path: wire_path,
        data_version: page.version.0,
        exists: page.exists,
        names: page
            .names
            .iter()
            .map(|name| name.as_str().to_owned())
            .collect(),
        next_page_token: page
            .next
            .map(|token| URL_SAFE_NO_PAD.encode(token.to_bytes())),
    })
}

pub(crate) async fn set_data(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    space: Space,
    request: SetDataRequest,
    parse_fence: impl FnOnce() -> Result<Option<OutboxFence>, ApiError>,
) -> Result<DataWriteResponse, ApiError> {
    let id = artifact_id(id)?;
    let path = data_path("path", &request.path)?;
    let value = data_value("value", &request.value)?;
    // Ordinary input validation retains its existing refusal before fence syntax.
    let fence = parse_fence()?;
    write_data(
        state,
        actor,
        key,
        (id, space),
        request.base_data_version,
        DataChange::Set { path, value },
        fence,
    )
    .await
}

pub(crate) async fn update_data(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    space: Space,
    request: UpdateDataRequest,
    parse_fence: impl FnOnce() -> Result<Option<OutboxFence>, ApiError>,
) -> Result<DataWriteResponse, ApiError> {
    let id = artifact_id(id)?;
    let path = data_path("path", &request.path)?;
    let merge = data_value("patch", &request.patch)?;
    // Ordinary input validation retains its existing refusal before fence syntax.
    let fence = parse_fence()?;
    write_data(
        state,
        actor,
        key,
        (id, space),
        request.base_data_version,
        DataChange::Merge { path, patch: merge },
        fence,
    )
    .await
}

pub(crate) async fn delete_data(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    space: Space,
    request: DeleteDataRequest,
    parse_fence: impl FnOnce() -> Result<Option<OutboxFence>, ApiError>,
) -> Result<DataWriteResponse, ApiError> {
    let id = artifact_id(id)?;
    let path = data_path("path", &request.path)?;
    // Ordinary input validation retains its existing refusal before fence syntax.
    let fence = parse_fence()?;
    write_data(
        state,
        actor,
        key,
        (id, space),
        request.base_data_version,
        DataChange::Remove { path },
        fence,
    )
    .await
}

/// Checks the shared space's access, declaration and writer role before a
/// write's key and content are read, so a member the declaration does not
/// let write learns nothing about them (decision 0060 rule 4). HTTP and MCP
/// call it before they read either; the write checks again where it commits
/// (rule 5). The caller's own space keeps its order: its role check is a
/// read, which the write makes where it commits.
pub(crate) async fn admit_space(
    state: &AppState,
    actor: &ActorId,
    id: &ArtifactId,
    space: Space,
) -> Result<(), ApiError> {
    if space == Space::Own {
        return Ok(());
    }
    let (actor, id) = (actor.clone(), id.clone());
    state
        .run(move |artifacts| artifacts.admit_data_write(&actor, &id, space))
        .await?;
    Ok(())
}

async fn write_data(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    (id, space): (ArtifactId, Space),
    base: u64,
    change: DataChange,
    fence: Option<OutboxFence>,
) -> Result<DataWriteResponse, ApiError> {
    let now = state.clock().now();
    let artifact = id.to_string();
    let path = match &change {
        DataChange::Set { path, .. }
        | DataChange::Merge { path, .. }
        | DataChange::Remove { path } => path.to_string(),
    };
    let outcome = state
        .run(move |artifacts| {
            artifacts.write_data_with_fence(
                &actor,
                &key,
                &id,
                DataMutation {
                    space,
                    base: AppDataVersion(base),
                    change: &change,
                    now,
                    fence: fence.as_ref(),
                },
            )
        })
        .await?;
    // A write that changed the space appended a `data_changed` row to the
    // actor's feed (decision 0040), or a `shared_data_changed` row to every
    // reader's (decision 0060 rule 13); wake the streams as a commit does. HTTP and
    // MCP both write through here. A replay, and a write that changed nothing,
    // recorded no row.
    if outcome.delivery == Delivery::Committed && outcome.changed {
        state.feed().committed();
    }
    Ok(DataWriteResponse {
        artifact_id: artifact,
        path,
        data_version: outcome.version.0,
        changed: outcome.changed,
        replayed: outcome.delivery == Delivery::Replayed,
    })
}

/// Lists the assets of an artifact, with the space's version and totals.
pub(crate) async fn list_assets(
    state: &AppState,
    actor: ActorId,
    id: &str,
) -> Result<AssetList, ApiError> {
    let artifact = artifact_id(id)?;
    let snapshot = state
        .run_assets(move |service| service.assets(&actor, &artifact))
        .await?;
    Ok(AssetList {
        asset_version: snapshot.version.0,
        assets: snapshot.assets.iter().map(asset_dto).collect(),
        bytes: snapshot.bytes,
        max_assets: MAX_SPACE_ASSETS,
        max_bytes: MAX_SPACE_BYTES,
    })
}

/// Uploads an asset under the retry key `key`. The base64 text is bounded
/// before it is decoded, and the decoded bytes are bounded before the store
/// sees them. The response is marked replayed when an earlier upload with the
/// same key is answered again.
pub(crate) async fn upload_asset(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    request: UploadAssetRequest,
) -> Result<UploadAssetResponse, ApiError> {
    let artifact = artifact_id(id)?;
    let base = asset_base(request.base_asset_version)?;
    let media: AssetMediaType = request
        .content_type
        .parse()
        .map_err(|error| ApiError::invalid(format!("content_type: {error}")))?;
    // Bound before allocation; STANDARD refuses URL-safe alphabets, missing padding,
    // noncanonical trailing bits and whitespace. Empty input belongs to content policy.
    let max_encoded = MAX_ASSET_BYTES.div_ceil(3) * 4;
    if request.content_base64.len() > max_encoded {
        return Err(ApiError::from(AssetStoreError::Rejected(
            AssetRejection::LimitExceeded,
        )));
    }
    let bytes = BASE64
        .decode(&request.content_base64)
        .map_err(|_| ApiError::invalid("content_base64 must be strict standard base64"))?;
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(ApiError::from(AssetStoreError::Rejected(
            AssetRejection::LimitExceeded,
        )));
    }
    let now = state.clock().now();
    let receipt = state
        .run_assets(move |service| {
            service.upload_asset(
                &actor,
                &key,
                &artifact,
                AssetUpload {
                    base,
                    content_type: &media,
                    bytes: &bytes,
                    now,
                },
            )
        })
        .await?;
    let metadata = receipt
        .uploaded
        .ok_or_else(|| ApiError::unavailable("upload receipt has no metadata"))?;
    Ok(UploadAssetResponse {
        asset_version: receipt.version.0,
        asset: asset_dto(&metadata),
        replayed: receipt.delivery == Delivery::Replayed,
    })
}

/// Reads one asset of an artifact: its metadata and exact bytes, with the
/// version of the space they were read at.
pub(crate) async fn asset_content(
    state: &AppState,
    actor: ActorId,
    id: &str,
    asset: &str,
) -> Result<AssetContent, ApiError> {
    let artifact = artifact_id(id)?;
    let asset = asset_id(asset)?;
    Ok(state
        .run_assets(move |service| service.asset_content(&actor, &artifact, &asset))
        .await?)
}

/// Deletes the association of an asset with an artifact under the retry key
/// `key`. The response is marked replayed when an earlier delete with the same
/// key is answered again.
pub(crate) async fn delete_asset(
    state: &AppState,
    actor: ActorId,
    key: RequestKey,
    id: &str,
    asset: &str,
    request: DeleteAssetRequest,
) -> Result<DeleteAssetResponse, ApiError> {
    let artifact = artifact_id(id)?;
    let asset = asset_id(asset)?;
    let base = asset_base(request.base_asset_version)?;
    let now = state.clock().now();
    let receipt = state
        .run_assets(move |service| service.delete_asset(&actor, &key, &artifact, base, &asset, now))
        .await?;
    Ok(DeleteAssetResponse {
        asset_id: receipt.id.as_str().to_owned(),
        asset_version: receipt.version.0,
        deleted: receipt.deleted,
        replayed: receipt.delivery == Delivery::Replayed,
    })
}

/// The wire form of an asset's metadata.
pub(crate) fn asset_dto(metadata: &AssetMetadata) -> Asset {
    let sha256 = hex(&metadata.digest);
    Asset {
        id: metadata.id.as_str().to_owned(),
        content_type: metadata.content_type.as_str().to_owned(),
        size_bytes: metadata.size,
        sha256,
        created_at_ms: metadata.created_at.0,
    }
}

/// Reads an asset base version. It must be a safe integer, at most 2^53 - 1,
/// so that a double-precision client holds it exactly.
pub(crate) fn asset_base(value: u64) -> Result<AssetVersion, ApiError> {
    if value > MAX_SAFE_INTEGER {
        return Err(ApiError::invalid(
            "base_asset_version must be a safe unsigned integer",
        ));
    }
    Ok(AssetVersion(value))
}

/// Reads a data base version. It must be a safe integer, at most 2^53 - 1, so
/// that a double-precision client holds it exactly.
pub(crate) fn data_base(value: u64) -> Result<AppDataVersion, ApiError> {
    if value > MAX_SAFE_INTEGER {
        return Err(ApiError::invalid(
            "base_data_version must be a safe unsigned integer",
        ));
    }
    Ok(AppDataVersion(value))
}

/// Converts a job record and the diagnostics a read shows with it to its
/// wire form.
///
/// The store returns only public jobs (decision 0058 rule 17): a hidden job
/// (decision 0057 rule 3) that reached here anyway is answered as not found,
/// never mapped onto a wire value.
fn job_dto(record: &JobRecord, diagnostics: &[BuildDiagnostic]) -> Result<WireJob, ApiError> {
    if !is_public_job(record.kind, record.profile.as_ref()) {
        return Err(ApiError::not_found("job not found"));
    }
    let kind = match record.kind {
        JobKind::VerifyRevision => WireJobKind::VerifyRevision,
        JobKind::RunProfile => WireJobKind::RunProfile,
    };
    let failure = record.failure.map(failure_dto);
    Ok(WireJob {
        id: record.id.to_string(),
        artifact_id: record.artifact.to_string(),
        revision: record.revision.0,
        kind,
        profile: record
            .profile
            .as_ref()
            .map(|profile| profile.as_str().to_owned()),
        state: match record.state {
            JobState::Queued => WireJobState::Queued,
            JobState::Running => WireJobState::Running,
            JobState::RetryScheduled => WireJobState::RetryScheduled,
            JobState::Succeeded => WireJobState::Succeeded,
            JobState::Failed => WireJobState::Failed,
            JobState::Cancelled => WireJobState::Cancelled,
        },
        cancel_requested: record.cancel_requested,
        attempt: u64::from(record.attempts),
        max_attempts: u64::from(record.max_attempts),
        created_at_ms: record.created_at.0,
        started_at_ms: record.started_at.map(|time| time.0),
        finished_at_ms: record.finished_at.map(|time| time.0),
        retry_at_ms: (record.state == JobState::RetryScheduled).then_some(record.run_after.0),
        failure,
        result: record.report.as_ref().map(|report| JobResult {
            verdict: if report.is_ok() {
                JobVerdict::Ok
            } else {
                JobVerdict::Mismatch
            },
            files_checked: report.files_checked,
            bytes_checked: report.bytes_checked,
            mismatch_count: report.mismatch_count,
            mismatches: report
                .findings
                .iter()
                .map(|finding| JobMismatch {
                    path: finding.path.to_string(),
                    problem: match finding.problem {
                        Problem::DigestMismatch => JobProblem::DigestMismatch,
                        Problem::SizeMismatch => JobProblem::SizeMismatch,
                        Problem::ContentMissing => JobProblem::ContentMissing,
                    },
                })
                .collect(),
            truncated: report.is_truncated(),
        }),
        build: record.counts.as_ref().map(|counts| JobBuild {
            file_count: counts.files,
            byte_count: counts.bytes,
        }),
        diagnostics: diagnostic_dtos(diagnostics),
    })
}

fn failure_dto(failure: JobFailure) -> WireFailure {
    match failure {
        JobFailure::RevisionUnavailable => WireFailure::RevisionUnavailable,
        JobFailure::StorageUnavailable => WireFailure::StorageUnavailable,
        JobFailure::LeaseExpired => WireFailure::LeaseExpired,
        JobFailure::Internal => WireFailure::Internal,
        JobFailure::ExecutionUnavailable => WireFailure::ExecutionUnavailable,
        JobFailure::BuildFailed => WireFailure::BuildFailed,
        JobFailure::LimitExceeded => WireFailure::LimitExceeded,
        JobFailure::OutputRejected => WireFailure::OutputRejected,
    }
}

fn diagnostic_dtos(diagnostics: &[BuildDiagnostic]) -> Vec<JobDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| JobDiagnostic {
            location: diagnostic
                .location
                .as_ref()
                .map(|location| DiagnosticLocation {
                    path: location.path.to_string(),
                    line: u64::from(location.line),
                    column: u64::from(location.column),
                }),
            message: diagnostic.message.as_str().to_owned(),
        })
        .collect()
}

fn commit_response(receipt: CommitReceipt) -> CommitResponse {
    CommitResponse {
        artifact: artifact_dto(&receipt.artifact),
        revision: revision_dto(receipt.revision),
        replayed: receipt.delivery == Delivery::Replayed,
    }
}

fn wire_kind(kind: ArtifactKind) -> WireKind {
    match kind {
        ArtifactKind::App => WireKind::App,
        ArtifactKind::Document => WireKind::Document,
        ArtifactKind::Slides => WireKind::Slides,
        ArtifactKind::Dashboard => WireKind::Dashboard,
        ArtifactKind::Design => WireKind::Design,
        ArtifactKind::Motion => WireKind::Motion,
    }
}

/// The wire form of an artifact's type (decision 0059 rule 8).
pub(crate) fn type_ref(pinned: &ArtifactType) -> ArtifactTypeRef {
    ArtifactTypeRef {
        id: pinned.id.to_string(),
        release: u64::from(pinned.release.get()),
    }
}

fn artifact_dto(record: &ArtifactRecord) -> Artifact {
    Artifact {
        id: record.id.to_string(),
        kind: wire_kind(record.kind),
        artifact_type: record.artifact_type.as_ref().map(type_ref),
        title: record.title.as_str().to_owned(),
        pinned: record.pinned,
        current_revision: record.current_revision.0,
        metadata_revision: record.metadata_revision.0,
        deleted_at_ms: record.deleted_at.map(|time| time.0),
        created_at_ms: record.created_at.0,
        updated_at_ms: record.updated_at.0,
        role: None,
        owner: None,
    }
}

/// The wire form of how a caller stands towards an artifact.
fn caller_role(access: Access) -> CallerRole {
    match access {
        Access::Owner => CallerRole::Owner,
        Access::Member(Role::Editor) => CallerRole::Editor,
        Access::Member(Role::Viewer) => CallerRole::Viewer,
    }
}

/// An artifact as a list shows it: with the caller's role and the owner
/// (decision 0060 rule 7).
fn listed_dto(listed: &PageArtifact) -> Artifact {
    Artifact {
        role: Some(caller_role(listed.access)),
        owner: Some(AccessUser {
            id: listed.record.owner.to_string(),
            label: listed.owner_label.as_str().to_owned(),
        }),
        ..artifact_dto(&listed.record)
    }
}

pub(crate) fn revision_dto(record: RevisionRecord) -> Revision {
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

    #[test]
    fn the_token_character_bound_follows_the_token_layout() {
        assert_eq!(MAX_TOKEN_BYTES, 80, "16 tag bytes and a 64-byte name");
        assert_eq!(MAX_MEMBER_TOKEN_CHARS, 107);
        // The bound is the length of the longest token as it is issued.
        assert_eq!(
            URL_SAFE_NO_PAD.encode(vec![0_u8; MAX_TOKEN_BYTES]).len(),
            MAX_MEMBER_TOKEN_CHARS
        );
    }

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

    fn receipt(delivery: Delivery) -> Result<CommitReceipt, Box<dyn Error>> {
        let artifact = ArtifactRecord {
            id: format!("art_{}", "a".repeat(32)).parse()?,
            owner: "owner".parse()?,
            kind: ArtifactKind::App,
            artifact_type: None,
            title: "Edit".parse()?,
            pinned: false,
            current_revision: SourceRevision(2),
            metadata_revision: MetadataRevision(1),
            deleted_at: None,
            created_at: UnixMillis(0),
            updated_at: UnixMillis(1),
        };
        let revision = RevisionRecord {
            revision: SourceRevision(2),
            created_by: "owner".parse()?,
            created_at: UnixMillis(1),
            files: Vec::new(),
        };
        Ok(CommitReceipt {
            artifact,
            revision,
            delivery,
        })
    }

    #[test]
    fn an_edit_wakes_the_change_streams_whenever_it_committed_a_revision()
    -> Result<(), Box<dyn Error>> {
        let cases: [(&str, Result<CommitReceipt, ArtifactError>, bool); 8] = [
            ("committed", Ok(receipt(Delivery::Committed)?), true),
            ("replayed", Ok(receipt(Delivery::Replayed)?), false),
            (
                "committed but not as asked",
                Err(ArtifactError::EditNotAsRequested),
                true,
            ),
            (
                "committed but not checked",
                Err(ArtifactError::EditUnverified),
                true,
            ),
            (
                "refused for its files",
                Err(ArtifactError::FilesChanged {
                    current: SourceRevision(2),
                    files: Vec::new(),
                    total: 1,
                }),
                false,
            ),
            (
                "store down before the commit",
                Err(ArtifactError::Unavailable("down".into())),
                false,
            ),
            ("key reused", Err(ArtifactError::RequestKeyReused), false),
            (
                "missing",
                Err(ArtifactError::NotFound(Missing::Artifact)),
                false,
            ),
        ];
        for (what, outcome, wakes) in cases {
            let feed = Feed::new(4);
            let heard = feed.wakeups();
            let answer = announce_edit(&feed, outcome);
            assert_eq!(
                heard.has_changed()?,
                wakes,
                "{what}: the streams are woken exactly when a revision was committed"
            );
            assert_eq!(
                answer.is_ok(),
                what == "committed" || what == "replayed",
                "{what}"
            );
        }
        Ok(())
    }

    #[test]
    fn an_edit_committed_but_not_verified_is_an_error_with_its_own_status() {
        assert_eq!(
            ApiError::from(ArtifactError::EditNotAsRequested).status,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ApiError::from(ArtifactError::EditUnverified).status,
            StatusCode::SERVICE_UNAVAILABLE
        );
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
            artifact_type: None,
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
