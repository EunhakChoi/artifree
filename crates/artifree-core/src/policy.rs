//! Authorization decisions. The server's authentication boundary establishes
//! the actor; an iframe-supplied actor or workspace identifier cannot.
//!
//! An artifact's owner may do everything with it; a member may do what its
//! role allows ([`Access::permits`], decision 0060 rule 3); every other actor
//! is refused as if the artifact did not exist (decision 0036). The store
//! reads the same rule again inside the transaction that commits a write
//! (0060 rule 5). The server separately checks the current connection grant
//! for each runtime capability a frame uses. The change feed applies the owner
//! rule in its storage query (`ArtifactService::changes`); members reach it
//! from a later slice (S02c).

use artifree_domain::access::{Access, Role};
use artifree_domain::identity::ActorId;
use artifree_ports::artifact_store::ArtifactRecord;

/// How `actor` stands towards `artifact`, given the member role the store
/// holds for it (none in the trash); `None` when the actor may not reach it.
#[must_use]
pub fn access_of(actor: &ActorId, artifact: &ArtifactRecord, role: Option<Role>) -> Option<Access> {
    if artifact.owner == *actor {
        Some(Access::Owner)
    } else {
        role.map(Access::Member)
    }
}
