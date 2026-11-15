# 0060: The access model for shared artifacts

Date: 2026-11-16. Status: **proposed (S02s, documentation only). No rule below
is implemented.** Roadmap: S02s, ahead of S02.

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
   owner).
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
   | Enqueue a job on it (verify, build); cancel one's own job                                                                | yes   | yes    | no                    |
   | Read and write one's own data and asset space (rule 9)                                                                   | yes   | yes    | yes                   |
   | Read the shared space (rule 10)                                                                                          | yes   | yes    | yes                   |
   | Write the shared space                                                                                                   | yes   | yes    | if declared `members` |
   | Read the member list (rule 6)                                                                                            | yes   | yes    | yes                   |
   | Grant, change or revoke a role; move to trash, restore from trash, purge                                                 | yes   | no     | no                    |

   The pin is artifact metadata, as now, so an editor's pin is seen by every
   member. A per-user pin is not part of S02.

4. **Refusals.** A user who is neither owner nor member, and every user while
   the artifact is in the trash except its owner, gets the answer an unknown
   name gets (0036 item 6, unchanged): `404`, the same body. A member who asks
   for an operation their role does not allow gets `403` with code `forbidden`
   and one body for every such case. The order is authentication, then the
   artifact's access (`404`), then the role (`403`), then the request body, so a
   viewer learns nothing about a write's body by sending it.
5. **The write path is unchanged.** Every source and metadata change by any
   member goes through `ArtifactService` with a base revision and an idempotency
   key, as now. Two members who commit on the same base conflict exactly as two
   tabs of one user do (`revision_conflict`, `file_conflict`,
   `metadata_conflict`). `created_by` records the member who committed, and the
   history shows that user's label to everyone who can read the artifact.

### Granting

6. **The access routes.** S02 adds:
   - `GET /v1/me`: the caller's user id and label, so a user can hand their id
     to an owner. It names no artifact and is open to every authenticated user.
   - `GET /v1/artifacts/{id}/access`: the owner (id and label), each member
     (id, label, role, and whether the user is disabled), and the artifact's
     `access_version`. Owner and members may read it.
   - `PUT /v1/artifacts/{id}/access/{user_id}` with `{ "role": … }` and
     `DELETE /v1/artifacts/{id}/access/{user_id}`: owner only. Each takes the
     `access_version` it was based on and an idempotency key in the owner's key
     space; a stale base is `409` with code `access_conflict`, and each change
     raises `access_version` by one. A user id that is not registered, is
     disabled, or is the owner is `invalid_request` with one message for all
     three, so the route does not tell a disabled user from an unknown one.
     Revoking a user who is not a member is `404`.
     The CLI gets `artifree artifact access list|grant|revoke` and `artifree me`.
     No MCP tool grants or revokes, by the same reasoning as 0036 item 3: an agent
     acting for a user does not widen who can reach that user's work. MCP read
     tools report the caller's role.
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
    artifact's **head** revision when a request is decided: removing it from
    the head makes the shared space unreachable (`404` on its routes) but keeps
    it, and declaring it again reaches it again. Routes and capabilities name
    the shared space explicitly (for example `space=shared`); nothing reaches it
    by default. An artifact with no members still has the space if it declares
    it; its owner is then its only reader.
11. **Builds belong to the artifact.** A build is keyed by artifact, revision
    and profile, so a build published by any member's job is the artifact's
    build, read by everyone who can read the artifact. The job that produced it
    stays the job of the user who enqueued it, and only that user lists,
    reads and cancels it.
12. **Rooms are per artifact.** The room of an artifact is shared by its owner
    and every member who joins it. A participant is identified by facts the
    server issues: the user id and label of the joining connection and a tab
    number the room hands out; a frame's own value never sets "this is me" or
    "same tab". The room's cap of participants is per room; the room budget
    (0034) stays charged per user. Nothing about a room is persisted, as now.

### Change delivery

13. **Each change reaches each user who can read it.** A change to an artifact's
    source, metadata, trash state, purge or shared space is written, in the same
    transaction as the change, into the feed of the owner and of every current
    member, each with its own `actor_seq`, so no user sees another user's
    numbering or a gap. A change to a user's own space and a job change reach
    only that user. A user whose access ends receives no later change of that
    artifact; rows they already received stay in their feed.
14. **Access changes are changes.** Granting, changing and revoking a role write
    one row each, of new kinds `access_granted`, `access_changed` and
    `access_revoked`, to the affected user's feed and to the owner's. A client
    that holds an open view of the artifact learns from them that its role
    changed.

### Revocation and its races

15. **What revocation closes.** Revoking a member closes for that user and that
    artifact (and only it), and the artifact moving to the trash closes for
    every member, as 0036 item 4 does for a disabled user: preview and build
    preview tickets, runtime views
    and runtime connections (closed and forgotten, with their grants and
    handoff offers), room participation, and handoff items that name the
    artifact. Change streams are per user and stay open; they carry the
    `access_revoked` row. A role change from editor to viewer closes that user's
    runtime connections to the artifact, so the next connection is granted
    within the narrower role. Queued jobs of a revoked user on that artifact are
    cancelled; a running one ends normally, and the build it publishes is the
    artifact's (rule 11).
16. **Registering after a revocation is closed by confirming after.** Each
    (artifact, member) has an `access_generation`, raised in the same statement
    that grants, changes the role of, or revokes the member, and when the
    artifact moves to or leaves the trash. Each registration that 0036 item 5
    confirms after registering is confirmed for the artifact it names against
    the generation the request saw, and a mismatch is answered and cleaned up as
    a revocation. What is tied to a runtime connection stays checked under the
    connection's lock (0036 item 5).
17. **Key replay checks access first.** A request key's stored answer is
    returned only after the caller's current access and role allow the
    operation again: a revoked user replaying a create's or commit's key gets
    the `404` of rule 4, and a viewer replaying an editor-era key gets `403`.
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
- revocation and downgrade closing each item of rule 15 over HTTP, MCP and an
  open frame, with a held registration raced across a revoke and a re-grant
  (rule 16), in Chromium and Firefox for the web parts;
- per-user spaces kept across revoke and re-grant, and the shared space's
  declaration, write policy and head-revision rule;
- feed delivery to each member with its own numbering, no delivery after
  revocation, and the access change rows;
- a room seen by two users with server-issued identity;
- key replay after revocation and after downgrade.

## Not part of this record

- Public or anonymous links, published addresses and a public viewer (S03).
- Groups, invitations, access requests, ownership transfer, per-file or
  per-path permissions, comments, and notifications outside the change feed.
- MCP tools that grant or revoke, and a web screen for users.
- A per-user pin, and moving an artifact's own space between users.
- Any statement about network isolation of previews (0003, unchanged).

## Implementation status

Nothing is implemented. S02 records its slices here as they merge.
