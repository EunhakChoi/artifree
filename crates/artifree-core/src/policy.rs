//! Authorization decisions. The server's authentication boundary establishes
//! the actor; an iframe-supplied actor or workspace identifier cannot.
//!
//! Only owner access exists today. Sharing, roles, runtime sessions, and
//! capability grants are planned and must be checked here when added.

use artifree_domain::identity::ActorId;
use artifree_ports::artifact_store::ArtifactRecord;

/// Whether `actor` may read and change `artifact`: only its owner may.
#[must_use]
pub fn may_access(actor: &ActorId, artifact: &ArtifactRecord) -> bool {
    artifact.owner == *actor
}
