# 0060: The access model for shared artifacts

Date: 2026-11-16. Status: **accepted as S02's design (S02s). S02a (the
stored part of rules 1, 6 and 16: members, roles, the access version and
the access generation, schema 18), S02b (the core's and the store's role
checks of rules 2 to 5, 9 and 17) and S02c (the feed of rules 13 and 14,
schema 19, contract 0.34.0) are implemented, as are the slices S02d1 to S02d4,
S02e1 (the access reads, contract 0.35.0), S02e2 (the owner's grant, role
change and revocation, contract 0.36.0), S02e3 (listings with the
caller's role, contract 0.37.0), S02f1 (the declared shared data space,
schema 20, contract 0.38.0) and S02f2 (the shared asset space, schema 21,
contract 0.39.0) that the implementation status lists; every other rule is
not implemented.**
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

| Slice | Rules                   | What it adds                                                                                                         | State                                   |
| ----- | ----------------------- | -------------------------------------------------------------------------------------------------------------------- | --------------------------------------- |
| S02a  | 1, 6 and 16 (storage)   | Schema 18: `artifact_members`, `access_requests`, `artifacts.access_version`; the `AccessStore` port                 | implemented                             |
| S02b  | 2 to 5, 9, 11, 17       | The core's access and role checks on every artifact operation, read again in the committing transaction              | implemented, except as its section says |
| S02c  | 13, 14                  | Feed delivery to every reader and the access change rows                                                             | implemented                             |
| S02d1 | 11, 15 (jobs), 17       | The queued jobs a revocation and a downgrade take, the owner's cancel, and member replays in the store               | implemented                             |
| S02d2 | 15 (memory, trash)      | What the trash removes from runtime memory, and the closure a revocation will call                                   | implemented                             |
| S02d3 | 16 (memory)             | The confirmations against the access generation of what a request registers                                          | implemented                             |
| S02d4 | 3 (runtime), 18 (store) | Runtime capabilities offered by role; the disabled mark the store returns                                            | implemented                             |
| S02e1 | 6 (reads), 8, 18 (mark) | `GET /v1/me` and `GET /v1/artifacts/{id}/access`, `artifree me` and `artifree artifact access list`, contract 0.35.0 | implemented                             |
| S02e2 | 6 (changes), 15         | The grant, role and revoke routes, the CLI's grant and revoke; what a revocation and a downgrade remove from memory  | implemented                             |
| S02e3 | 6 (MCP), 7              | Listings with the caller's role, the owner and a filter; the role in MCP read tools                                  | implemented                             |
| S02f1 | 10 (data), 13           | The declared shared data space: its declaration, routes, tools and feed kind; schema 20, contract 0.38.0             | implemented                             |
| S02f2 | 10 (assets)             | The shared asset space: its holder, the uploader's charge, routes, tools and feed kind; schema 21, contract 0.39.0   | implemented                             |
| S02f3 | 10 (runtime)            | The shared space in runtime frames                                                                                   | not implemented                         |
| S02g  | 12                      | Rooms per artifact                                                                                                   | not implemented                         |
| S02h  | Acceptance (web)        | The web workspace's sharing                                                                                          | not implemented                         |

Nothing reached a member's access before S02e2 exposed the routes that
grant, and S02e waited for S02b to S02d4, so no slice opens an artifact to another user without
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
viewer's frame is shown `source.write` as not available (S02d4; the
downgrade's grant removal is S02e) — until then a viewer's runtime source write is
refused by the role check like any other write; and the owner's cancel of a
member's job (rule 11) (S02d1); cancelling a
revoked or downgraded member's queued jobs, which until then a worker can
still claim, and a store-side access read for the replays the service
answers itself (a write whose type release this server does not know, and a
copy whose source is gone), both with the revocation (S02d1); and the order
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
feed of the user whose space or job it is; a job of a revoked member keeps
running (S02b) but writes no more rows, so the revocation stays that user's
last row. Schema 19 rebuilds the change
table, keeping every row and number, for the new kinds `access_granted`,
`access_changed` and `access_revoked` (rule 14): the store appends one in
the transaction of each access change that changes something, to the owner
and every member after the change and, for a revocation, to the revoked
user, whose last row of the artifact it is. The upgrade to schema 19 runs
with foreign keys off, as the job rebuilds do (decision 0057 rule 14), so a
row an older file holds whose key does not resolve is kept, and the step may
break no key that was whole. Contract 0.34.0 adds the three kinds to the
wire `ChangeKind`; the TypeScript client knows them, and the web workspace,
which has no sharing yet (S02h), takes each as news of the artifact to read
again, as it does a kind it does not know. Behavioural tests over a real
database (`crates/artifree-providers/tests/access_feed.rs`) and of the web
reducer (`scripts/tests/access-change-reducer.test.mjs`) fail when each
guard is broken. A purge deletes the member records before it records
`purged`, so no request observes the trash branch of the fan-out; a unit
test of the store asks it directly. No route makes these rows reachable before S02e, which also wakes
open streams after an access change.

**S02d1 (implemented).** A revocation, and a role change from editor to
viewer, end as cancelled the jobs that user queued on the artifact and that
have not started (queued or scheduled for a retry), in the transaction of the
access change; a running job ends normally (rule 15). Whatever puts a job back in line
afterwards (its worker releasing it, a retry, an expired lease) meets the
claim, which ends as cancelled, without an attempt, a due job whose user is
neither the owner nor an editor of the artifact, so no new attempt starts for
a user who may no longer enqueue there. Each cancel is a job
change, which reaches the user's feed only while it is still a member, so
for a revoked user the revocation stays the last row. The owner of an
artifact cancels a member's job on it (rules 3 and 11), and the answer
carries the job as a member's own cancel does; another member does not find
it. Listing and reading a job stay the enqueuing user's. The store's
replay of a stored commit, which the service uses where it answers a retry
itself (a release this server does not know, a copy whose source is gone),
reads the caller's access in the same snapshot as the key: a member whose
role no longer allows a commit gets `forbidden`, a revoked one `not_found`
(rule 17); the key of a create or a copy names an artifact the caller owns,
so its replay is unchanged. Behavioural tests over a real database
(`crates/artifree-providers/tests/revocation_jobs.rs`) fail when each guard
is broken. What a revocation and the trash remove from runtime memory is
S02d2; the confirmations against the access generation (rule 16) are S02d3;
runtime capabilities offered by role and rule 18's marking are S02d4 (now
implemented).

**S02d2 (implemented).** A move to the trash ends, for each current member
and that artifact only, what the server holds in memory (rule 15): its
preview, build preview and runtime view tickets of the artifact, the grants
scoped to it on each of the member's runtime connections, its participants
in its room of the artifact, and its handoff deliveries that name it,
offered or not. The connections, the change streams, the inboxes and
everything of other artifacts stay; the fences of the closed views stay
too, since they grant nothing and keep an older generation from being issued
again. The store names the members in the transaction that commits the
move, in the metadata receipt, and the server closes what they hold right
after it, over HTTP and MCP alike; a replay names no member and closes
nothing, so a retried move after a recovery leaves what members opened
again. The owner's own tickets, grants, room and handoffs of the artifact
are left as they were before S02. The same closure, for one user, is what a
revocation runs, but no route revokes a member yet: the call comes with the
access routes (S02e). Behavioural tests over the real registries and service
(`runtime_tests.rs`: `a_move_to_the_trash_closes_what_a_member_holds_of_that_artifact_only`
and `a_replayed_move_to_the_trash_closes_nothing_after_a_recovery`) fail when
each guard is broken. Not in S02d2: a request that passed its access check
before the move may still register afterwards, until the confirmations of
rule 16 (S02d3, now implemented); the revocation's call (S02e); and what a downgrade to viewer
removes (the `source.write` grants), which comes with the route that changes
a role (S02e).

**S02d3 (implemented).** Each registration that a request makes for an
artifact and that outlives it is confirmed against the access generation
(rule 16): a preview or build preview link, a runtime view, a runtime grant
that holds a capability, a room join and a staged handoff. The request
reads the caller's generation on the artifact before its access check,
registers, and then reads it again. Any change in between (a revocation, a
role change, or a move to or out of the trash) is answered as a revocation:
what the caller holds of the artifact is closed (the closure of S02d2, for
that user), and the request gets the `404` of an unknown artifact. A read
that fails confirms nothing, so it closes the same way and answers with its
error. A caller that still has access asks again. A grant that withdraws
every capability grants nothing, so it takes effect at once and is not
confirmed. The generation is read through
`ArtifactService::access_generation`, which returns only the caller's own.
A change that commits after the second read is cleaned up by its own
closure, which runs after its commit. Behavioural tests in
`runtime_tests.rs`, one per registration (`*_across_a_revocation_*`) plus
`a_preview_link_whose_confirmation_cannot_read_is_closed`, revoke the
viewer in the store, or make the database unreadable, between the
registration and the confirmation. They fail when each guard is broken. Not
in S02d3: runtime capabilities offered by role and rule 18's mark (S02d4),
and the routes that change access (S02e).

**S02d4 (implemented).** Runtime capabilities follow the caller's role
(rule 3). A caller who may not change the artifact, a viewer, is offered
every capability except `source.write`. Its runtime view leaves
`source.write` out of `supported`, so the workspace shows a declared
`source.write` as not available. A grant that asks for it is not refused,
but `source.write` is left out of what is granted; the next runtime view
reports what was granted. A view reports a grant only as the caller's role
offers it now, so a grant an editor held before a downgrade never shows
`source.write` beside a `supported` without it. The role is read with the
runtime view's or grant's access check through `ArtifactService::allows`.
Every runtime source write still checks the current role in the committing
transaction (S02b), so a grant that an editor holds when downgraded writes
nothing; removing that grant from memory comes with the role route (S02e).
Rule 18's mark is the `disabled` flag that the S02a store already returns
with each member (`a_disabled_member_stays_a_member_and_its_role_can_change`
in `crates/artifree-providers/tests/access.rs`); the access list that marks
them for the owner is S02e1's route. A disabled member's grants are kept, and nothing
they hold authenticates while they are disabled (S01). Behavioural tests
(`a_viewer_is_offered_every_capability_but_source_write`, which walks the
owner, an editor and a viewer, and
`a_downgraded_editors_view_reports_only_what_a_viewer_is_offered`, in
`runtime_tests.rs`, and a unit test in `runtime_policy.rs`) fail when each
guard is broken. Not in S02d4: the shared space's write capabilities as
rule 10 declares them (S02f).

**S02e1 (implemented).** The reads of rule 6. `GET /v1/me` answers the caller's id and label to every authenticated caller, an agent token of the owner profile included; it names no artifact. `GET /v1/artifacts/{id}/access` answers the owner (id and label), the members ordered by user id (id, label, role) and the `access_version`, through `ArtifactService::access`, which checks the caller's access as every read does (rule 4): a user who is neither owner nor member, and a member while the artifact is in the trash, gets the `404` of an unknown artifact; the owner reads it in the trash too. Access is checked again against the list, which the store reads in one snapshot with whether the artifact is in the trash, so a member revoked, or an artifact moved to the trash, between the first check and the read is not found rather than shown the list. Each member carries `disabled`, which is rule 18's mark in the owner's answer and `null` in a member's, so a member learns no more about another member than rule 8 allows. The store reads the labels with the list, in one transaction; an owner whom no registry holds, which 0036 lets a database name, is listed under its id. The core decides what each caller is told, so every adapter shows the same. Contract 0.35.0 adds `Me`, `ArtifactAccess` and the wire `MemberRole`; the TypeScript client gains `readMe` and `readAccess`, and the CLI `artifree me` and `artifree artifact access list <id> [--json]`. The isolation table of `multi_user.rs` walks the access route as another user. Behavioural tests (`crates/artifree-server/tests/access_reads.rs`, `the_access_list_is_checked_again_against_its_own_snapshot` in the core, `the_list_names_the_owner_and_whether_the_artifact_is_in_the_trash` in `crates/artifree-providers/tests/access.rs`, and `scripts/tests/access-reads.test.mjs` for the client's parsers and the CLI) fail when each guard is broken. Not in S02e1: the routes that grant, change or revoke a role, and what a revocation or a downgrade removes from memory (S02e2); listings with the caller's role, and the role in MCP read tools (S02e3).

**S02e2 (implemented).** The changes of rule 6, and what rule 15 removes from memory. `PUT /v1/artifacts/{id}/access/{user_id}` with `{ "role", "base_access_version" }` grants a role or changes it, and `DELETE /v1/artifacts/{id}/access/{user_id}` with `{ "base_access_version" }` revokes it, each through `ArtifactService::change_access` with an `Idempotency-Key` in the owner's key space. The credential rule is purge's: `RequireAccessChange` refuses a token issued to the owner profile with `403` before the key, the identifiers or the body are read. Then the order of rule 4: an artifact the caller cannot reach, or the owner's in the trash, is the `404` of an unknown artifact; a member is `403` before its body is read; then the body. A stale base is `409` `access_conflict` with `current_access_version`; a user id that is not registered, is disabled, or is the owner's, and a spelling no user can have, is one `invalid_request`; revoking a user who is not a member is `404`. A replayed key returns the first answer with `replayed: true`, and another change under it is `request_key_reused`. When the change commits, a revocation runs the closure of S02d2 for the revoked user and that artifact (tickets, the artifact's grants on each of the user's connections, the room, handoffs), and a grant of `viewer` narrows the artifact's grants on the user's connections to what a viewer is offered, so `source.write` goes and everything else stays. A replay removes nothing: what it would remove went when it committed, and anything held since was granted after it. The store's transaction already cancels the queued jobs (S02d1), raises the access generation (S02a, closing registrations made across the change, S02d3) and appends the change rows (S02c). Contract 0.36.0 adds `SetAccessRequest`, `RevokeAccessRequest`, `AccessChangeResponse`, the error code `access_conflict` and `current_access_version`; the client gains `setAccess` and `revokeAccess`, and the CLI `artifree artifact access grant|revoke`. The isolation table walks the route as another user, as the administrator and as the owner profile's agent. Behavioural tests (`crates/artifree-server/tests/access_changes.rs`; `a_revocation_closes_what_the_member_holds_of_that_artifact_only`, `a_replayed_revocation_closes_nothing_after_a_new_grant` and `a_downgrade_narrows_the_artifacts_grants_and_keeps_the_rest` in `runtime_tests.rs`; `scripts/tests/access-changes.test.mjs`) fail when each guard is broken. Not in S02e2: listings with the caller's role, and the role in MCP read tools (S02e3); the shared space and its declaration's writes on a downgrade (S02f); rooms per artifact (S02g); the web workspace (S02h).

**S02e3 (implemented).** Rule 7's listings and rule 6's role in the read tools. `GET /v1/artifacts` and the MCP tool `list_artifacts` list the caller's own artifacts and, outside the trash, those they are a member of, each with `role` (`owner`, `editor` or `viewer`) and `owner` (id and label; an owner no registry holds is listed under its id). `access=owned` or `access=shared` keeps one or the other; the trash (`deleted=true`) lists only the caller's own, so `access=shared` there is empty. The store reads the list, the roles and the owners' labels in one query: a member's rows come from the membership index, a revoked member's row has no role and lists nothing, and an artifact in the trash lists no member. Pages and the token follow the same rule: `access` is part of the filters a token continues, so an owned list's token does not continue the shared list, and a list without the filter keeps the tokens it had. `GET /v1/artifacts/{id}` and the MCP tool `get_artifact` report the caller's `role`; the answers of writes carry neither field, and only a list names the owner. Contract 0.37.0 adds `CallerRole`, `ListAccess`, `ListArtifactsQuery.access`, and `Artifact.role` and `Artifact.owner`; the client keeps both fields and refuses a malformed one, and the CLI gains `artifree artifact list --owned|--shared`. Until S02h shows roles, the web workspace lists only the caller's own artifacts (`access=owned`), because every row offers an owner's actions. Behavioural tests (`a_member_lists_what_they_own_and_what_is_shared_with_them`, `the_owner_is_listed_by_label_or_by_id` and `pages_continue_across_owned_and_shared_artifacts` in `crates/artifree-providers/tests/access.rs`; the token case in `listing.rs`; `a_member_finds_the_shared_artifact_in_their_list` in `crates/artifree-server/tests/access_changes.rs`; `read_tools_report_the_callers_role_and_the_owner` in `mcp_adapter.rs`; `scripts/tests/access-listings.test.mjs`) fail when each guard is broken. Not in S02e3: the role in the other read tools, which answer about revisions, files and jobs rather than the artifact; the shared space (S02f); rooms per artifact (S02g); the web workspace's roles and shared filter (S02h).

**S02f1 (implemented).** Rule 10 for application data, and rule 13 for its changes. `artifree-runtime.json` with `"shared": "editors"` or `"shared": "members"` declares the shared space; the domain's `SharedWriters::declared_in` reads the manifest (at most 16 KiB of UTF-8 holding a JSON object) and refuses any other value of `shared`, and the runtime policy then refuses the manifest as malformed. The store reads the declaration from the head revision's manifest inside the transaction of every shared read and write, so a manifest without the field, or one that cannot be read, reaches no shared space: its routes answer `404` while the rows stay, and declaring it again reaches them again. Schema 20 adds `shared_data` (one document and `data_version` per artifact, with the bounds of a user's space) and the change kind `shared_data_changed`, which carries `data_version` like `data_changed`; the upgrade rebuilds the change table with the foreign keys relaxed, as schema 19's did, and keeps every row. The data routes take `space=shared` (`GET`, `PUT`, `PATCH` and `DELETE /v1/artifacts/{id}/data`, and `GET .../data/members`), and the MCP tools `get_data`, `set_data`, `update_data` and `delete_data` take `"space": "shared"`; without it they name the caller's own space, as before, and a write's query refuses any parameter but `space`. Rule 17's order holds: access is checked first (an artifact the caller cannot reach is `404`), then the declaration (`404` without one), then the writer the declaration names (`editors`: the owner and editors; `members`: every member and the owner; anyone else is `403 forbidden`), and only then the key. HTTP and MCP run these checks before they read a shared write's key and body, so a refused writer learns nothing of either (rule 4), and the store runs them again where the write commits (rule 5); a shared read checks the caller's access in the snapshot it reads. A key stays in the writer's key space, and the request it replays names its space, so the same key sent to the other space is `request_key_reused`; a member-listing page token names its space too. A shared write appends `shared_data_changed` to the feed of the owner and of every current member; a write to a user's own space still reaches only that user. A purge erases the shared space with the artifact. Contract 0.38.0 adds `DataSpaceName`, `DataWriteQuery`, the `space` field of the data queries and tool arguments, and the change kind; the client gains `space` on `getData`, `listDataMembers` and the three writes and parses the new kind, the CLI gains `artifree data ... --shared`, and the web workspace takes a shared change, like a data change, as nothing to read again. Behavioural tests (`crates/artifree-providers/tests/shared_data.rs`; `crates/artifree-server/tests/shared_data.rs`; the declaration test in `artifree-domain`'s `access.rs`; the token cases in `artifree-core`'s `members.rs`; `scripts/tests/shared-data.test.mjs`, and the shared cases in `client.test.mjs` and `access-change-reducer.test.mjs`) fail when each guard is broken. Not in S02f1: the shared asset space (S02f2); the shared space in runtime frames, whose data capability still reaches only the caller's own space (S02f3); a duplicate does not copy the shared space, as `--copy-data` copies only the caller's own; rooms per artifact (S02g); the web workspace (S02h).

**S02f2 (implemented).** Rule 10 for uploaded assets, and rule 13 for their changes. The shared asset space exists while the head revision declares the shared space, with the same declaration and the same writers as the shared data space (S02f1); without it its routes answer `404` while its assets stay. Its rows are the rows of `assets` and `asset_spaces` held by the reserved holder `*shared`, which no user id can spell, so the space has its own `asset_version` and its own limits (64 assets and 25 MiB, as a user's space). Schema 21 adds `assets.charged_to`, the user a shared upload is charged to (empty for a user's own asset): each shared upload counts against its uploader's own limits across every artifact (256 assets and 100 MiB) as well as against the shared space's, and against no other member's; a delete frees both. Schema 21 also adds the change kind `shared_assets_changed`, which carries no version; the upgrade rebuilds the change table as schema 20's did and keeps every row. The asset routes take `space=shared` (`GET` and `POST /v1/artifacts/{id}/assets`, `GET` and `DELETE .../assets/{asset_id}`, and `GET .../assets/{asset_id}/content`), and the MCP tools `list_assets`, `read_asset`, `upload_asset` and `delete_asset` take `"space": "shared"`; without it they name the caller's own space, as before, and every asset route's query refuses any parameter but `space`. The checks run in the order of S02f1: access (`404`), the declaration (`404`), the writer the declaration names (`403 forbidden`), then the key; HTTP and MCP run them before they read a shared write's key and body (rule 4), and the store runs them again in the transaction where the write commits, before key replay (rules 5 and 17), so a writer the head no longer names is refused rather than answered from its earlier receipt. A shared read checks the caller's access in its snapshot. A key stays in the uploader's key space, and the request it replays names its space, so the same key sent to the other space is `request_key_reused`. A shared upload, and a shared delete that removed an asset, append `shared_assets_changed` to the feed of the owner and of every current member and wake their change streams; a write to a user's own asset space still adds no feed row. A purge erases the shared space with the artifact, and a duplicate with `copy_assets` copies only the caller's own space. Contract 0.39.0 adds `AssetQuery`, the `space` field of the asset tool arguments, and the change kind; the client gains `space` on `listAssets`, `getAsset`, `readAsset`, `uploadAsset` and `deleteAsset` and parses the new kind, the CLI gains `artifree asset ... --shared`, and the web workspace takes a shared asset change as nothing to read again. Behavioural tests (`crates/artifree-providers/tests/shared_assets.rs`; `crates/artifree-server/tests/shared_assets.rs`; `scripts/tests/shared-assets.test.mjs`, and the shared cases in `asset-mutation-client.test.mjs`, `client.test.mjs` and `access-change-reducer.test.mjs`) fail when each guard is broken. Not in S02f2: the shared space in runtime frames, whose asset capabilities still reach only the caller's own space (S02f3); rooms per artifact (S02g); the web workspace (S02h).
