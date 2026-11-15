# 0060: The access model for shared artifacts

Date: 2026-11-16. Status: **accepted as S02's design (S02s). S02a (the
stored part of rules 1, 6 and 16: members, roles, the access version and
the access generation, schema 18), S02b (the core's and the store's role
checks of rules 2 to 5, 9 and 17) and S02c (the feed of rules 13 and 14,
schema 19, contract 0.34.0) are implemented; no route grants a role yet, so
no request reaches them as a member, and every other rule is not
implemented.**
Roadmap: S02s, S02.

> Builds on [0036](0036-multi-user-identity.md) (users, tokens, isolation and
> the state generation), [0006](0006-change-feed.md) and
> [0040](0040-data-changes-and-member-listing.md) (the change feed),
> [0020](0020-runtime-grants.md) (runtime capabilities and current authority),
> [0029](0029-retention-and-cleanup.md) (trash and purge),
> [0031](0031-app-data.md) and [0045](0045-uploaded-assets.md) (application
> data and assets), [0034](0034-room-and-presence.md) (rooms),
> [0030](0030-persistent-jobs-and-worker.md) and
> [0058](0058-react-builds-and-preview.md) (jobs and builds), and
> [0051](0051-roadmap-and-architecture-review.md) (finding 6 and Q4, which set
> this record's direction).

Independent Artifree design: no private evidence was consulted, and nothing
below is presented as an observation of the original product. The roles, the
routes, the shared space and the delivery rules are Artifree's own decisions.

A code pull request may not change a numbered rule; an amendment is a separate
documentation pull request.

## Context

Since S01 every artifact has exactly one user who can reach it: its owner
(`artifacts.owner`, the user who created it). Everything else that is stored is
bound to a user too:

- **To the user and the artifact:** application data (`app_data`), the asset
  space (`asset_spaces`, `assets`), room membership (in memory).
- **To the user alone:** request keys (`request_keys`, `data_requests`,
  `asset_requests`, `job_requests`, `cleanup_runs`), jobs (`jobs.actor`), the
  change feed's numbering (`changes.actor`, `actor_seq`, `actor_feeds`), and the
  per-user bounds of 0036 item 9.
- **To the artifact alone:** revisions and their files (with `created_by`),
  metadata revisions (title, pin, `created_by`), the trash state, the type, and
  builds (`builds`, `build_files`, keyed by artifact, revision and profile).

Another user's artifact answers exactly as an unknown name does (0036 item 6).
The administrator is not a reader.

0051 Q4 fixed the direction: sharing an artifact shares its source; each user
who reaches it keeps their own data and asset space; state that several users
see is a separate space the artifact declares. This record fills in what Q4
left to S02s: the roles and what each allows, the declared shared space, the
scope of rooms, who receives which change, how request keys behave across
sharing and revocation, and what revocation closes. It changes no code or
schema. S02 implements it, possibly in several pull requests.

## Rules

### Members and roles

1. **One owner, then members.** The owner of an artifact is the user who created
   it, for its whole life. Ownership is not transferred. The owner may make
   another registered, enabled user a **member** of the artifact with one role,
   `editor` or `viewer`. A user is a member at most once per artifact; changing
   the role replaces the grant. The owner is never a member of their own
   artifact. An artifact has at most 31 members (0036's 32 users, less the
   owner). In this record "member" means a user granted access to an
   artifact; it is unrelated to the data member names of 0040 and to the
   `members[]` of a room in 0034.
2. **What a grant is not.** A grant is not part of a revision or a metadata
   revision. Restoring an older revision or metadata revision never changes who
   has access. Duplicating an artifact creates a new artifact owned by the user
   who duplicated it, with no members.
3. **What each role allows.** "Read" includes every view of the stored source.

   | Operation                                                                                                                | Owner | Editor | Viewer                |
   | ------------------------------------------------------------------------------------------------------------------------ | ----- | ------ | --------------------- |
   | Find it in listings; read the artifact, revisions, files, history, export, type; preview; build status and build preview | yes   | yes    | yes                   |
   | Duplicate it (rule 2)                                                                                                    | yes   | yes    | yes                   |
   | Commit source (save, per-file edits, restore as a new revision); change title or pin                                     | yes   | yes    | no                    |
   | Enqueue a job on it (verify, build)                                                                                      | yes   | yes    | no                    |
   | Cancel a job on it: one's own (any role, rule 11), or any member's (owner)                                               | yes   | own    | own                   |
   | Read and write one's own data and asset space (rule 9)                                                                   | yes   | yes    | yes                   |
   | Read the shared space (rule 10)                                                                                          | yes   | yes    | yes                   |
   | Write the shared space                                                                                                   | yes   | yes    | if declared `members` |
   | Read the member list (rule 6)                                                                                            | yes   | yes    | yes                   |
   | Grant, change or revoke a role; move to trash, restore from trash, purge                                                 | yes   | no     | no                    |

   The pin is artifact metadata, as now, so an editor's pin is seen by every
   member. A per-user pin is not part of S02.

   Runtime surfaces follow the same table. A connection, a preview or build
   preview ticket, a runtime view, a room join and a handoff that names the
   artifact are open to every role. Of the runtime capabilities (0020),
   `source.write` is granted only to owner and editors; `metadata.read`,
   `source.read`, `source.download` and the `data.*` and `assets.*`
   capabilities on the caller's own space are granted to every role; on the
   shared space reads are granted to every role and writes as rule 10's
   declaration says. A capability the role does not allow is shown as not
   available and is never granted; asking for it is not an error, and every
   runtime call checks the current role again (0020 rule 4).

4. **Refusals.** A user who is neither owner nor member, and every user while
   the artifact is in the trash except its owner, gets the answer an unknown
   name gets (0036 item 6, unchanged): `404`, the same body. A member who asks
   for an operation their role does not allow gets `403` with code `forbidden`
   and one body for every such case. The order is authentication, then the
   artifact's access (`404`), then the role (`403`), then the request body, so a
   viewer learns nothing about a write's body by sending it.
5. **The write path is unchanged, and access is checked where the write
   commits.** Every source and metadata change by any member goes through
   `ArtifactService` with a base revision and an idempotency key, as now. The
   caller's access and role, and for the shared space its declaration (rule
   10), are read again inside the transaction that commits a source, metadata,
   data, asset, job or access change, so a write that passed its checks before
   a revoke, a downgrade or a move to the trash commits nothing after it and
   answers as rule 4 does at that moment. Unlike 0036 item 5's late writes to a
   user's own data, a late write into another user's artifact is not kept. Two members who commit on the same base conflict exactly as two
   tabs of one user do (`revision_conflict`, `file_conflict`,
   `metadata_conflict`). `created_by` records the member who committed, and the
   history shows that user's label to everyone who can read the artifact.

### Granting

6. **The access routes.** S02 adds:
   - `GET /v1/me`: the caller's user id and label, so a user can hand their id
     to an owner. It names no artifact and is open to every authenticated user.
   - `GET /v1/artifacts/{id}/access`: the owner (id and label), each member
     (id, label, role), and the artifact's `access_version`. Owner and members
     may read it; only the owner's answer also says which members are
     disabled.
   - `PUT /v1/artifacts/{id}/access/{user_id}` with `{ "role": … }` and
     `DELETE /v1/artifacts/{id}/access/{user_id}`: owner only. Each takes the
     `access_version` it was based on and an idempotency key in the owner's key
     space; a stale base is `409` with code `access_conflict`, and each change
     raises `access_version` by one. Granting to a user id that is not
     registered, is disabled, or is the owner is `invalid_request` with one
     message for all three, so the route does not tell a disabled user from an
     unknown one; changing the role of an existing member who is disabled is
     allowed (rule 18). Revoking a user who is not a member is `404`.
   - **Who may call the grant routes.** As `RequirePurge` does for purge (0036
     item 13): the owner's bootstrap token, when the owner is the owner
     profile, and a token issued to a user other than the owner profile; a
     token issued to the owner profile is the agent credential of 0029 and is
     refused with `403` before the key, the identifier or the body is read. No
     MCP tool grants or revokes. **The cost is the same as 0036 item 13's:** a
     user other than the owner profile has no other credential, and the server
     cannot tell a person's token from an agent's, so an agent holding such a
     user's token can share that user's artifacts over HTTP or the CLI.
     The CLI gets `artifree artifact access list|grant|revoke` and `artifree me`.
     MCP read tools report the caller's role.
7. **Listings.** `GET /v1/artifacts` lists the caller's own artifacts and those
   they are a member of, each with the caller's role and the owner's id and
   label; a filter selects owned or shared ones. The trash lists only the
   caller's own artifacts. Counts and pages follow the same rule.
8. **Who learns what about whom.** An owner and all members of one artifact see
   each other's user id, label and role there, and the label of whoever
   committed a revision. Nothing else about a user is revealed, and a user who
   shares nothing with another learns nothing about them. The administrator
   still reads nothing of a user's (0036 item 6).

### Per-user and shared state

9. **Per-user state stays per user (0051 Q4).** Each member who reaches an
   artifact has their own application data document and asset space at
   (artifact, user), as the owner does. Request keys, cleanup runs, jobs, the
   change feed's numbering and the bounds of 0036 item 9 stay keyed by user. A
   member's own space is kept when the member is revoked, is unreachable while
   they are not a member, and is reachable again if they are granted again.
   Purge removes every user's space of the artifact. The HTTP and MCP data and
   asset routes and R14's frame capability keep addressing the caller's own
   space unless rule 10's space is named.
10. **The declared shared space.** An artifact may declare one shared space in
    its runtime manifest (`artifree-runtime.json`, 0020 rule 1) with one field,
    `"shared": "editors"` or `"shared": "members"`, naming who may write it;
    every member and the owner may read it. Any other value makes the manifest
    malformed. The space is one application data document and one asset space
    at (artifact, shared), with the same size bounds, `data_version` and
    `asset_version` conditions and key replay rules as a user's space; its
    request keys are in the key space of the user who sent the write, and its
    budgets are charged to that user. The declaration is read from the
    artifact's **head** revision inside the transaction that commits the write
    (rule 5), so a write never lands under a policy the head no longer has:
    removing it from
    the head makes the shared space unreachable (`404` on its routes) but keeps
    it, and declaring it again reaches it again. Routes and capabilities name
    the shared space explicitly (for example `space=shared`); nothing reaches it
    by default. An artifact with no members still has the space if it declares
    it; its owner is then its only reader.
11. **Builds belong to the artifact.** A build is keyed by artifact, revision
    and profile, so a build published by any member's job is the artifact's
    build, read by everyone who can read the artifact. The job that produced it
    stays the job of the user who enqueued it, and only that user lists and
    reads it; that user and the artifact's owner may cancel it.
12. **Rooms are per artifact.** The room of an artifact is shared by its owner
    and every member who joins it. A participant is identified by facts the
    server issues: the user id and label of the joining connection and a tab
    number the room hands out; a frame's own value never sets "this is me" or
    "same tab". The room keeps its cap of 16 participants (0034), and no user
    holds more than 4 of them, so one member cannot fill the room; 4 of the 16
    places are kept for the owner. The room budget (0034) stays charged per
    user. Nothing about a room is persisted, as now.

### Change delivery

13. **Each change reaches each user who can read it.** A change to an artifact's
    source, metadata or shared space is written, in the same transaction as the
    change, into the feed of the owner and of every current member, each with
    its own `actor_seq`, so no user sees another user's numbering or a gap. A
    change to a user's own space and a job change reach only that user. A user
    whose access ends receives no later change of that artifact; rows they
    already received stay in their feed. The trash follows from rule 4: the
    move to the trash (`deleted`) reaches the owner and every member, and is a
    member's last row of the artifact; while it is in the trash only the owner
    receives its rows, including `purged`; a restore (`recovered`) reaches the
    owner and every member, and is a member's first row after the trash.
14. **Access changes are changes.** Granting, changing and revoking a role write
    one row each, of new kinds `access_granted`, `access_changed` and
    `access_revoked`, to the owner, to every current member (who can all read
    the member list, rule 6) and to the user whose access was revoked. A
    client that holds an open view of the artifact learns from them that its
    role or the member list changed.

### Revocation and its races

15. **What revocation closes.** Revoking a member removes, for that user and
    that artifact only, and the artifact moving to the trash removes for every
    member: preview and build preview tickets and runtime views of the
    artifact; on each of the user's runtime connections, the grants scoped to
    the artifact and the handoff offers that name it (a connection is per user
    and may hold grants for other artifacts, which stay); the user's
    participation in the artifact's room; and handoff items that name the
    artifact. A connection itself is not closed by a revocation, unlike 0036
    item 4's disable. Change streams are per user and stay open; they carry the
    `access_revoked` row. Queued jobs of the revoked user on that artifact are
    cancelled; a running one ends normally, and the build it publishes is the
    artifact's (rule 11).
    **A role change from editor to viewer** removes less: on each of the
    user's runtime connections, the artifact's grants of capabilities a viewer
    is not granted (rule 3: `source.write`, and shared-space writes when the
    declaration is `editors`); and the user's queued jobs on the artifact,
    since a viewer may not enqueue. Tickets, views, room participation,
    handoffs and the other grants stay. A role change from viewer to editor
    removes nothing.
16. **Registering after a revocation is closed by confirming after.** Each
    (artifact, user) that was ever a member has an `access_generation`, raised
    in the same statement that grants, changes the role of, or revokes the
    member, and when the artifact moves to or leaves the trash. It only grows:
    a revocation keeps the (artifact, user) record with no role, and a later
    grant raises the kept value, so a registration that saw an earlier grant
    never matches a later one. Only purge removes the record. Each registration that 0036 item 5
    confirms after registering is confirmed for the artifact it names against
    the generation the request saw, and a mismatch is answered and cleaned up as
    a revocation. What is tied to a runtime connection stays checked under the
    connection's lock (0036 item 5).
17. **Key replay checks access first.** A request key's stored answer is
    returned only after the caller's current access and role allow the
    operation again: a revoked user replaying the key of a commit, metadata
    change, data or asset write or job on the artifact gets the `404` of rule
    4, and a viewer replaying an editor-era key gets `403`. Creating and
    duplicating make an artifact the caller owns, so their replays are
    unchanged.
    Nothing about the stored answer is revealed by either.
18. **A disabled member stays a member.** Disabling a user (0036 item 4) keeps
    their grants; nothing they hold authenticates while they are disabled, and
    the access list marks them. Enabling them restores their access as granted.

## Acceptance for S02

S02 is complete only with behavioural tests, proven by mutation, for:

- every route and MCP tool that names an artifact, walked as a non-member (the
  `404` of an unknown name, same body), as a viewer and as an editor (each
  `403` where rule 3 says no, and success where it says yes);
- two members committing on one base (one conflict), and `created_by` with its
  label in history;
- grant, role change and revoke with a stale `access_version`, a replayed key,
  and an unknown, disabled or owner user id (one answer);
- a revocation removing each item rule 15 lists for it, and a downgrade
  removing exactly the items rule 15 lists for a downgrade and keeping the
  rest, over HTTP, MCP and an open frame, with a connection that also holds
  another artifact's grant keeping it; with a held registration raced across a
  revoke and a re-grant not matching (rule 16); in Chromium and Firefox for the web parts;
- per-user spaces kept across revoke and re-grant, and the shared space's
  declaration, write policy and head-revision rule;
- feed delivery to each member with its own numbering, no delivery after
  revocation, and the access change rows;
- a room seen by two users with server-issued identity, and one user's
  participant limit;
- key replay after revocation and after downgrade;
- a write held past a revoke, a downgrade and a change of the shared space's
  declaration committing nothing (rule 5);
- the web workspace: the owner shares, changes a role and revokes from the
  artifact's page; a member sees the artifact under a shared filter with
  their role, and the page offers only what the role allows; in Chromium and
  Firefox.

## Not part of this record

- Public or anonymous links, published addresses and a public viewer (S03).
- Groups, invitations, access requests, ownership transfer, per-file or
  per-path permissions, comments, and notifications outside the change feed.
- Roles other than owner, editor and viewer; MCP tools that grant or revoke;
  and a web screen for managing users.
- A per-user pin, and moving an artifact's own space between users.
- Any statement about network isolation of previews (0003, unchanged).

## Implementation status

S02 is split into slices, each its own pull request, in this order:

| Slice | Rules                 | What it adds                                                                                             | State                                   |
| ----- | --------------------- | -------------------------------------------------------------------------------------------------------- | --------------------------------------- |
| S02a  | 1, 6 and 16 (storage) | Schema 18: `artifact_members`, `access_requests`, `artifacts.access_version`; the `AccessStore` port     | implemented                             |
| S02b  | 2 to 5, 9, 11, 17     | The core's access and role checks on every artifact operation, read again in the committing transaction  | implemented, except as its section says |
| S02c  | 13, 14                | Feed delivery to every reader and the access change rows                                                 | implemented                             |
| S02d  | 15, 16 (memory), 18   | What a revocation, a downgrade and the trash remove, and the confirmations against the access generation | not implemented                         |
| S02e  | 6 to 8                | `/v1/me`, the access routes, listings with roles, the CLI and the contract                               | not implemented                         |
| S02f  | 10                    | The declared shared space                                                                                | not implemented                         |
| S02g  | 12                    | Rooms per artifact                                                                                       | not implemented                         |
| S02h  | Acceptance (web)      | The web workspace's sharing                                                                              | not implemented                         |

Nothing reaches a member's access before S02e exposes the routes, and S02e
waits for S02b to S02d, so no slice opens an artifact to another user without
the checks, the delivery and the revocation that go with it.

**S02a (implemented).** Schema 18 adds `artifact_members` (one row per
artifact and user that was ever a member: the role, or none once revoked,
and an access generation that only grows), `access_requests` (the owner's
request keys of access changes, in the shared key space) and
`artifacts.access_version`. The `AccessStore` port reads the list, a
member's role and generation, and changes one user's access conditional on
the version, with replay; it refuses the owner, an unknown user and a
disabled user who is not a member alike, and a revocation of a non-member.
A move to or from the trash raises every member's generation, and a purge
deletes the rows and blanks the access keys' fingerprints. No route, tool
or core operation uses it yet. Behavioural tests on a real database
(`crates/artifree-providers/tests/access.rs`) fail when each guard is
broken.

**S02b (implemented, except as said here).** The domain names what an
operation needs (`Need`: read, change, own) and what each stand allows
(`Access::permits`, rule 3's table); the core and the store apply the same
table. `ArtifactService` resolves the caller as owner, member with the role
the store holds (none in the trash), or nobody: nobody gets `not_found`, a
member whose role does not allow the operation gets the new
`ArtifactError::Forbidden`, which HTTP and MCP answer as `403` `forbidden`
with one body; the service checks the role before it looks at the
operation's content (rule 4). Reads and
duplication need a read, commits, restores, title and pin changes and job
enqueueing need a change, and a move to or from the trash and a purge need
the owner. Inside the transaction that commits a revision, a file edit, a
metadata change, a purge, a copy, a data write, a job or a job's cancel, the
store reads the
caller's role again before the request key (rules 5 and 17): a write that
passed the core's check before a downgrade or a revocation commits nothing,
and a replay of an editor-era key by a viewer is `forbidden`, by a revoked
member `not_found`. A member lists and cancels its own jobs only while it is
a member. Data and asset spaces stay keyed by (artifact, user), so
a member reaches its own space while it is a member (rule 9); a copy is the
caller's and copies the caller's own space (rule 2). A running job of a
revoked member is no longer a change of authority (rule 15's running job):
only an actor who was never owner or member is. Behavioural tests over a
real database (`crates/artifree-providers/tests/access_writes.rs`,
`crates/artifree-server/tests/member_roles.rs`, and a case in
`run_profile_claim.rs`) fail when each guard is broken.

Not in S02b, and moved to the slice named: listing an artifact one is a
member of, with its role (S02e); runtime capabilities offered by role, so a
viewer's frame is shown `source.write` as not available (S02d, with the
downgrade's grant removal) — until then a viewer's runtime source write is
refused by the role check like any other write; and the owner's cancel of a
member's job (rule 11), with the job reads of the owner (S02d); cancelling a
revoked or downgraded member's queued jobs, which until then a worker can
still claim, and a store-side access read for the replays the service
answers itself (a write whose type release this server does not know, and a
copy whose source is gone), both with the revocation (S02d); and the order
of the HTTP and MCP adapters, which parse and validate a request body before
the service checks the role, so a viewer's malformed write is answered
`invalid_request` rather than `403` until the routes are opened to members
(S02e). Rule 11's
build sharing needs nothing new: a build is already keyed by artifact,
revision and profile, and a member reads it through the read check.

**S02c (implemented).** A change of the artifact itself (created, revised,
metadata, trash, recovery, purge) is appended, in its transaction, to the
feed of the owner and of every current member, each with its own
`actor_seq`; while the artifact is in the trash only the owner's feed gets
rows, except the move into the trash, which every member gets as its last
row of the artifact (rule 13). A data change and a job change stay in the
feed of the user whose space or job it is. Schema 19 rebuilds the change
table, keeping every row and number, for the new kinds `access_granted`,
`access_changed` and `access_revoked` (rule 14): the store appends one in
the transaction of each access change that changes something, to the owner
and every member after the change and, for a revocation, to the revoked
user, whose last row of the artifact it is. Contract 0.34.0 adds the three
kinds to the wire `ChangeKind`; the TypeScript client knows them, and the
web workspace, which has no sharing yet (S02h), treats them as a change that
names the artifact's current revision. Behavioural tests over a real
database (`crates/artifree-providers/tests/access_feed.rs`) fail when each
guard is broken, except the trash branch of the fan-out: a purge deletes the
member records before it records `purged`, so no request observes that
branch. No route makes these rows reachable before S02e, which also wakes
open streams after an access change.
