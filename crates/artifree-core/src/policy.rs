//! Authorization decisions. The server's authentication boundary establishes
//! the actor; an iframe-supplied actor or workspace identifier cannot.
//!
//! Only owner artifact access exists today. Sharing and roles are planned
//! and must extend this decision. The server separately checks the current
//! connection grant for each runtime metadata read. The change feed applies
//! the owner rule in its storage query (`ArtifactService::changes`), so
//! sharing must extend that query too.

use artifree_domain::identity::ActorId;
use artifree_ports::artifact_store::ArtifactRecord;

/// Whether `actor` may read and change `artifact`: only its owner may.
#[must_use]
pub fn may_access(actor: &ActorId, artifact: &ArtifactRecord) -> bool {
    artifact.owner == *actor
}
