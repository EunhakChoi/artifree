//! The schema of each released version, as the migration ladder created it.
//!
//! Every historical build created a fresh database by running these steps in
//! order, so a database built here from the steps up to version N is what that
//! build produced. The texts are frozen copies, not references to production
//! code: changing the live ladder must not change what an old release wrote.
//! The access token table is what the token store created beside schema
//! version 5, before it joined the versioned schema.

pub const STEP_1: &str = "
CREATE TABLE artifacts (
    id TEXT PRIMARY KEY,
    owner TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    current_revision INTEGER NOT NULL CHECK (current_revision >= 1),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE INDEX artifacts_by_owner ON artifacts (owner, updated_at DESC, id);
CREATE TABLE revisions (
    artifact_id TEXT NOT NULL REFERENCES artifacts (id),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (artifact_id, revision)
) STRICT, WITHOUT ROWID;
CREATE TABLE blobs (
    digest BLOB PRIMARY KEY CHECK (length(digest) = 32),
    content BLOB NOT NULL
) STRICT;
CREATE TABLE revision_files (
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    path TEXT NOT NULL,
    size INTEGER NOT NULL CHECK (size >= 0),
    digest BLOB NOT NULL REFERENCES blobs (digest),
    PRIMARY KEY (artifact_id, revision, path),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision)
) STRICT, WITHOUT ROWID;
CREATE TABLE request_keys (
    actor TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, request_key),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision)
) STRICT, WITHOUT ROWID;
";

/// Added in schema version 2. `sqlite_sequence` keeps the largest sequence
/// ever issued, so the head survives future pruning of old rows.
pub const STEP_2: &str = "
CREATE TABLE feed (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    id BLOB NOT NULL CHECK (length(id) = 16)
) STRICT;
CREATE TABLE changes (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('created', 'revised')),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision)
) STRICT;
";

/// Added in schema version 3: titles and pins per metadata revision, and the
/// metadata revision of each request key and change. Existing artifacts get
/// metadata revision 1 with their current title, unpinned. Tables whose
/// constraints change are rebuilt; `sqlite_sequence` keeps the feed's head.
pub const STEP_3: &str = "
ALTER TABLE artifacts ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1));
ALTER TABLE artifacts ADD COLUMN metadata_revision INTEGER NOT NULL DEFAULT 1
    CHECK (metadata_revision >= 1);
CREATE INDEX artifacts_by_owner_created ON artifacts (owner, created_at DESC, id);
CREATE TABLE metadata_revisions (
    artifact_id TEXT NOT NULL REFERENCES artifacts (id),
    metadata_revision INTEGER NOT NULL CHECK (metadata_revision >= 1),
    title TEXT NOT NULL,
    pinned INTEGER NOT NULL CHECK (pinned IN (0, 1)),
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (artifact_id, metadata_revision)
) STRICT, WITHOUT ROWID;
INSERT INTO metadata_revisions (artifact_id, metadata_revision, title, pinned, created_by, created_at)
    SELECT id, 1, title, 0, owner, created_at FROM artifacts;
CREATE TABLE request_keys_v3 (
    actor TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    metadata_revision INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, request_key),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    FOREIGN KEY (artifact_id, metadata_revision)
        REFERENCES metadata_revisions (artifact_id, metadata_revision)
) STRICT, WITHOUT ROWID;
INSERT INTO request_keys_v3
    SELECT actor, request_key, fingerprint, artifact_id, revision, 1, created_at FROM request_keys;
DROP TABLE request_keys;
ALTER TABLE request_keys_v3 RENAME TO request_keys;
CREATE TABLE changes_v3 (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('created', 'revised', 'metadata_updated')),
    revision INTEGER NOT NULL,
    metadata_revision INTEGER NOT NULL,
    committed_at INTEGER NOT NULL,
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    FOREIGN KEY (artifact_id, metadata_revision)
        REFERENCES metadata_revisions (artifact_id, metadata_revision)
) STRICT;
INSERT INTO changes_v3 (sequence, artifact_id, kind, revision, metadata_revision, committed_at)
    SELECT c.sequence, c.artifact_id, c.kind, c.revision, 1, r.created_at
    FROM changes c
    JOIN revisions r ON r.artifact_id = c.artifact_id AND r.revision = c.revision;
DROP TABLE changes;
ALTER TABLE changes_v3 RENAME TO changes;
";

/// Added in schema version 4: the latest commit time, which the next commit
/// must exceed. It starts at the latest time any earlier commit stored, also
/// when stored times are out of order or have no feed entry.
pub const STEP_4: &str = "
ALTER TABLE feed ADD COLUMN committed_at INTEGER NOT NULL DEFAULT 0;
UPDATE feed SET committed_at = coalesce((SELECT max(at) FROM (
    SELECT max(created_at) AS at FROM revisions
    UNION ALL SELECT max(created_at) FROM artifacts
    UNION ALL SELECT max(updated_at) FROM artifacts
    UNION ALL SELECT max(created_at) FROM metadata_revisions
    UNION ALL SELECT max(committed_at) FROM changes
)), 0);
";

/// Added in schema version 5: when an artifact went to the trash, also per
/// metadata revision, and the two change kinds of the trash. The change table
/// is rebuilt for its checked kinds.
pub const STEP_5: &str = "
ALTER TABLE artifacts ADD COLUMN deleted_at INTEGER;
ALTER TABLE metadata_revisions ADD COLUMN deleted_at INTEGER;
CREATE TABLE changes_v5 (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT NOT NULL,
    kind TEXT NOT NULL
        CHECK (kind IN ('created', 'revised', 'metadata_updated', 'deleted', 'recovered')),
    revision INTEGER NOT NULL,
    metadata_revision INTEGER NOT NULL,
    committed_at INTEGER NOT NULL,
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    FOREIGN KEY (artifact_id, metadata_revision)
        REFERENCES metadata_revisions (artifact_id, metadata_revision)
) STRICT;
INSERT INTO changes_v5 (sequence, artifact_id, kind, revision, metadata_revision, committed_at)
    SELECT sequence, artifact_id, kind, revision, metadata_revision, committed_at FROM changes;
DROP TABLE changes;
ALTER TABLE changes_v5 RENAME TO changes;
";

/// The token table as the token store created it before schema version 6.
pub const OLD_TOKENS: &str = "
CREATE TABLE IF NOT EXISTS tokens (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    secret_hash TEXT NOT NULL UNIQUE,
    prefix TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS idx_tokens_secret_hash ON tokens (secret_hash);
";

/// Added in schema version 7 (decision 0029): the tombstone of an artifact
/// removed for good, the record of a cleanup of unreferenced content, an index
/// on the content a revision file refers to, and the change kind of a purge.
pub const STEP_7: &str = "
ALTER TABLE artifacts ADD COLUMN purged_at INTEGER
    CHECK (purged_at IS NULL OR deleted_at IS NOT NULL);
CREATE INDEX revision_files_by_digest ON revision_files (digest);
CREATE TABLE cleanup_runs (
    actor TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    blobs INTEGER NOT NULL CHECK (blobs >= 0),
    bytes INTEGER NOT NULL CHECK (bytes >= 0),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, request_key)
) STRICT, WITHOUT ROWID;
CREATE TABLE changes_v7 (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
        ('created', 'revised', 'metadata_updated', 'deleted', 'recovered', 'purged')),
    revision INTEGER NOT NULL,
    metadata_revision INTEGER NOT NULL,
    committed_at INTEGER NOT NULL,
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    FOREIGN KEY (artifact_id, metadata_revision)
        REFERENCES metadata_revisions (artifact_id, metadata_revision)
) STRICT;
INSERT INTO changes_v7 (sequence, artifact_id, kind, revision, metadata_revision, committed_at)
    SELECT sequence, artifact_id, kind, revision, metadata_revision, committed_at FROM changes;
DROP TABLE changes;
ALTER TABLE changes_v7 RENAME TO changes;
";

/// Added in schema version 8 (decision 0030): persistent jobs, the keys of the
/// requests that enqueued and cancelled them, and the mismatches a
/// verification found.
pub const STEP_8: &str = "
CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    actor TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('verify_revision')),
    state TEXT NOT NULL CHECK (state IN
        ('queued', 'running', 'retry_scheduled', 'succeeded', 'failed', 'cancelled')),
    attempts INTEGER NOT NULL CHECK (attempts >= 0),
    max_attempts INTEGER NOT NULL CHECK (max_attempts >= 1),
    fence INTEGER NOT NULL CHECK (fence >= 0),
    run_after INTEGER NOT NULL,
    worker TEXT,
    lease_expires_at INTEGER,
    cancel_requested INTEGER NOT NULL CHECK (cancel_requested IN (0, 1)),
    failure TEXT CHECK (failure IN
        ('revision_unavailable', 'storage_unavailable', 'lease_expired', 'internal')),
    files_checked INTEGER CHECK (files_checked >= 0),
    bytes_checked INTEGER CHECK (bytes_checked >= 0),
    mismatch_count INTEGER CHECK (mismatch_count >= 0),
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    CHECK (attempts <= max_attempts),
    CHECK ((state = 'running') = (worker IS NOT NULL AND lease_expires_at IS NOT NULL)),
    CHECK ((state IN ('succeeded', 'failed', 'cancelled')) = (finished_at IS NOT NULL)),
    CHECK ((state = 'failed') = (failure IS NOT NULL)),
    CHECK (state = 'succeeded'
        OR (files_checked IS NULL AND bytes_checked IS NULL AND mismatch_count IS NULL))
) STRICT;
CREATE INDEX jobs_by_actor ON jobs (actor, created_at DESC, id);
CREATE INDEX jobs_by_artifact ON jobs (artifact_id, created_at DESC, id);
CREATE INDEX jobs_by_state ON jobs (state, run_after);
CREATE TABLE job_requests (
    actor TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    job_id TEXT NOT NULL REFERENCES jobs (id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, request_key)
) STRICT, WITHOUT ROWID;
CREATE TABLE job_findings (
    job_id TEXT NOT NULL REFERENCES jobs (id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    path TEXT NOT NULL,
    problem TEXT NOT NULL CHECK (problem IN
        ('digest_mismatch', 'size_mismatch', 'content_missing')),
    PRIMARY KEY (job_id, ordinal)
) STRICT, WITHOUT ROWID;
";

/// Added in schema version 9 (decision 0031): application data and the keys
/// of data writes.
pub const STEP_9: &str = "
CREATE TABLE app_data (
    artifact_id TEXT NOT NULL REFERENCES artifacts (id),
    actor TEXT NOT NULL,
    data_version INTEGER NOT NULL CHECK (data_version >= 1),
    document TEXT NOT NULL CHECK (json_valid(document) AND json_type(document) = 'object'),
    PRIMARY KEY (artifact_id, actor)
) STRICT;
CREATE TABLE data_requests (
    actor TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    artifact_id TEXT NOT NULL REFERENCES artifacts (id),
    data_version INTEGER NOT NULL CHECK (data_version >= 0),
    changed INTEGER NOT NULL CHECK (changed IN (0, 1)),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, request_key)
) STRICT, WITHOUT ROWID;
";
