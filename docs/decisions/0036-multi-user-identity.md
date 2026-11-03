# 0036: Multi-user identity

Date: 2026-11-03. Status: accepted. Builds on
[0023](0023-single-owner-token-management.md) (tokens, the bootstrap
administrator, cascading revocation),
[0006](0006-change-feed.md) (the change feed),
[0020](0020-runtime-grants.md) and [0034](0034-room-and-presence.md) (runtime
connections and rooms), and [0027](0027-backup-restore-and-schema-upgrade.md)
(the schema ladder). Roadmap row: S01. Independent Artifree design: the row
asks for a request token to map to a real actor and for create, read, write and
data scope to be verified with two or more users while the local owner profile
keeps working. The registry, the ids, the routes and the isolation rules below
are Artifree's own decisions; nothing below is an observation of the original
product, and no role combination of the original is assumed.

## Context

Until now every token authenticated as one actor, `local`. The core already
scoped artifacts, jobs, data, request keys and rooms by actor, but nothing
could create a second actor, and several things that were meant to be per actor
were in fact process-wide: the change feed's numbering, the room cap, the
numbers rooms hand out, and the bounds on runtime connections, preview tickets
and handoffs. With one actor those were indistinguishable from per-actor
bounds. With two they are a leak (one user's activity moves another's numbers)
and a denial of service (one user fills the places).

This record is S01: **a persistent registry of users, tokens that belong to
exactly one of them, and isolation between them on every surface.** It is not
S02 (roles, sharing, per-object grants) and implements none of it.

## Implemented

1. **A persistent registry.** A `users` table (schema 10) holds `id`, a unique
   `label` (1-64 characters, no control characters, no surrounding space),
   `created_at`, `disabled_at` and `state_generation` (item 11). The owner is user number 1 with the id it
   always had, `local`, label `Local owner`; the migration inserts it, so an
   older database opens with the owner already registered. Every other user's id
   is `u-` followed by 32 random hexadecimal digits. Both spellings are valid
   `ActorId`s (decision 0003's grammar is unchanged), and only these two
   spellings can be registered. At most 32 users exist, the owner included.
   There is no deletion and no rename: a user is disabled, never removed.
2. **A token belongs to exactly one user, for its whole life.** `tokens.actor`
   is a required foreign key to `users`. A token is issued to a registered,
   enabled user; the migration gives every existing token to `local`. Rotation
   keeps the user. A token whose user row is missing does not list or
   authenticate. The bootstrap token (the file token) is the owner's and the only
   administrator; tokens that were issued, including those issued to the owner
   profile, are never administrators.
3. **Who creates users and issues tokens.** Only the administrator
   (`RequireAdmin`, `403` for anyone else, decided after authentication and
   before the path or body is read): `GET/POST /v1/users`,
   `POST /v1/users/{id}/disable`, `POST /v1/users/{id}/enable`, and the
   existing token routes, where `POST /v1/tokens` takes an optional `user_id`
   (default: the owner profile; an unknown, disabled or unspellable user is an
   error before anything is stored). The CLI has `artifree user list|create|
disable|enable` and `artifree token create <name> --user <id>`. No MCP tool
   exists for users or tokens, and none is added: an agent token cannot create a
   user or a token. There is no web screen.
4. **What disabling does.** `disabled_at` is set (the first time is kept; the
   owner cannot be disabled, `409`). From then on every token of the user is the
   same `401` as a token nobody issued, on HTTP and MCP. The server also closes
   what it holds open in memory for the user: change streams, preview tickets
   and runtime view fences, runtime connections (closed **and forgotten**, with
   their grants and handoff offers), room participants, and registered handoff
   inboxes. A closed connection is removed, not kept marked: a kept entry would
   count against the user's bound of 4096 until its lifetime ended, and a user
   at the bound would find every new connection refused after being enabled
   again. Nothing stored is touched: artifacts, revisions, data, jobs and the
   user's tokens stay, and the tokens are not revoked. **Enabling lets the
   tokens that are not revoked authenticate again; what disabling closed stays
   closed**, so a client opens a new connection, stream or link. The
   administrator can still list, revoke and rotate the tokens of a disabled
   user: a revoked token stays revoked when the user is enabled, and a rotated
   secret authenticates only once the user is enabled (a rotation is one
   transaction, and its answer states whether the user is disabled at that
   moment). A revoked token's cascade (streams, tickets, connections) is
   unchanged.
5. **Registering after the disable is closed by confirming after.** A request
   that authenticated before a disable can register something that outlives it
   after the disable's cleanup ran. Every registration of long-lived per-user
   state in memory was inventoried, and each is one of two kinds:
   - **Confirmed after registering** (`ensure_current`, item 11: it reads the
     user and, if it is disabled, gone, or no longer in the state generation the
     request authenticated under, runs the cleanup again and answers `401`, so
     either the request sees the changed user and removes its own registration,
     or the disable's cleanup comes later and removes it): change streams,
     preview links, runtime views, runtime connections
     (`POST /v1/runtime-sessions`), and agent inboxes (MCP
     `register_handoff_session`). The HTTP and MCP handlers call only the
     `*_confirmed` forms, and a test fails when a handler calls the registering
     function alone.
   - **Tied to a runtime connection and checked under its lock**: room
     participants, staged handoff deliveries, grants. A connection closes before
     the rooms and deliveries of the user are emptied, and each of these changes
     its state only while the connection is open (`while_active`), so a call
     that passed its lookup before the disable is refused afterwards and none
     outlives its connection.
     Stored data is not state of this kind: an artifact, revision, data document or
     job written by a request that authenticated before the disable is the user's
     data like any other, and is there when the user is enabled again.
6. **Isolation: another user's name is a name that does not exist.** An artifact,
   revision, file, export, trashed artifact, job, application data, preview,
   runtime view or grant, room, handoff or inbox that belongs to another user
   answers exactly what the same call on an unknown name answers (`404`, the same
   body). That holds in the core (`owned`/`authorized`), on every artifact-scoped
   HTTP route and every MCP tool that names one, and on the frame and room
   routes, whose bindings include the actor. Listings, trash listings, job lists
   and counts contain only the caller's own. The administrator is not a reader:
   the bootstrap token is the owner profile's actor, so it sees the owner's
   artifacts and nothing of a user's. A test walks every route and tool with
   another user's real names and with names nobody holds and requires the same
   status and body, and a second test reads the server's route declarations and
   fails when a route is neither in the table nor on the short list of routes
   that name no artifact.
7. **Request keys are per user.** Every key space (`request_keys`, cleanup runs,
   job requests, data requests) is scoped by `(actor, key)` in the store, so the
   same key text from two users is two requests, another user's replay is never
   returned, and reusing a key with a different body is a conflict only for the
   user who used it.
8. **The change feed is per user.** `changes` gets `actor` and `actor_seq`, with
   the head per actor in `actor_feeds`; the global `sequence` stays the primary
   key and the write order. A cursor is `feed.<actor_seq>`. The migration copies
   the old sequence into `actor_seq` and gives each actor that has changes the
   old head, so a single user's cursors stay valid and the next change continues
   after the head. A stream reads only its own actor's rows, so another user's
   commit is neither delivered nor visible as a gap in numbering. Every row of
   the old feed is kept: the upgrade fails, changing nothing, if the rebuilt
   feed would hold fewer rows, and a row whose artifact row is missing (which
   only a file edited outside Artifree can hold, since foreign keys are enforced
   and no artifact row is ever deleted; a purged artifact keeps its row) goes to
   the owner profile.
9. **Bounds that were global are per user.** Rooms with a participant
   (`max_rooms`), the numbers rooms hand out (participant, tab, epoch), runtime
   connections (4096), preview tickets, view fences and pending creations (4096
   each), handoff inboxes (128) and items (512). The request limiter, the three
   request budgets and the open-stream cap were already keyed by actor; they stay.
   Each is tested for "one user at the bound, another unaffected", and for "a
   user at the bound has its places back after a disable and an enable".
10. **Schema 10 and contract 0.14.0.** One transaction: `users` (with the owner
    row, a `CHECK` that the owner is never disabled, and `CHECK`s on the id
    spelling), `tokens` rebuilt with `actor`, `actor_feeds`, `changes` rebuilt
    with `actor`/`actor_seq`. The feed head is carried over. The structural check
    that compares a database with a fresh one, and the restore path of decision
    0027, include the new tables and the `users.state_generation` column. `TokenInfo.user_id`,
    `CreateTokenRequest.user_id`, `UserInfo`, `CreateUserRequest`, `UserList`
    are new in the generated contract.
11. **A disable that was undone is still a change: the state generation.** The
    confirmation of item 5 alone sees "enabled now". A request that authenticated
    while a user was enabled, was delayed, and registers something after the
    administrator disabled the user (the cleanup ran) and enabled it again would
    keep a registration the disable was meant to close. So `users.state_generation`
    (`INTEGER NOT NULL DEFAULT 1 CHECK (>= 1)`, one for every user at registration
    and for the owner for ever) is raised by one **in the same statement** that
    changes `disabled_at`, on a disable and on an enable that change the state; a
    repeated disable or enable changes nothing and does not raise it. It is stored
    in the database, so it survives a restart, and the only statement that writes
    it adds one (a file edited outside Artifree is outside the guarantee); it is read
    in the same row as the token (the token query joins `users`), so a token
    authenticates together with the generation its user has at that instant, never
    from two reads. The generation travels in the request's `AuthContext` to
    every handler that registers state, and `AppState::ensure_current` compares
    the user's _current_ generation with it, together with "registered and
    enabled", after the registration. A mismatch is the same `401` as a disabled or
    unknown token (indistinguishable), and runs the cleanup again.
    - **Why both transitions.** A disable alone would separate the two epochs for
      this race, since nothing authenticates while a user is disabled. Raising it
      on both makes the number change on every change of state, so one equality
      says "same enabled period, nothing in between" and any rule written later
      against the number cannot rely on the half that was left out. It costs one
      integer.
    - **Where it is checked.** Streams (`GET /v1/changes`), preview links,
      runtime views, runtime connections and agent inboxes, over HTTP and MCP,
      all through `ensure_current`; a source test counts the confirmations and
      fails if a handler registers without one or uses a check that only reads
      "enabled". Room participants, staged deliveries and grants have no
      generation of their own: they change state only through the runtime
      connection they hold, the disable closes and removes the connection, and a
      removed connection is never active again, so a delayed call that holds one
      is refused whatever the user's state is by then (tested with a disable
      followed by an enable).
    - **A refusal closes more than the late request's own registration.**
      Registrations carry no owner of their own, so the cleanup that follows a
      refusal removes everything the user holds, including what it opened under
      the newer generation in the meantime. Those clients see a closed stream or
      connection and open it again. This is an availability cost of a rare race,
      accepted over tagging every registry entry with a generation.
      Narrowing the refusal to the late request's own registration was
      re-examined after this record (follow-up to S01) and **not changed**: it
      needs a removal for one registration in each of five registries (change
      streams, preview tickets, runtime views, runtime connections, handoff
      inboxes), and only the connection registry has one by id. The cleanup is
      also the right answer whenever the user is disabled or gone, which is
      most refusals; only "enabled again under a newer generation" would gain,
      and that is the rare case already accepted above.
    - **Pending view creations and fences.** `revoke_actor` drops the fences of an
      actor's views even while a create for one is pending. The create from the
      old epoch is refused anyway (its connection is closed, and the generation
      check is the second line), and removes whatever it registered, so keeping
      the fence would only keep an entry for a request that cannot succeed. A test
      holds a create pending across a disable and an enable and shows it refused,
      with nothing left and the same view issuable afterwards.
    - **Not covered.** The generation guards in-memory registrations that outlive
      a request. It does not guard stored data: an artifact, revision, data
      document or job written by a request that authenticated before the
      disable is the user's data (item 5). A job queued before the disable is
      not started while the user is disabled (item 12). A token an administrator issues is created
      only for a user that is enabled in the same statement, so a racing disable
      cannot leave an active token. The owner has no generation to compare
      because it is never disabled.

12. **Follow-up: a disabled user's queued jobs are not started.** The set of
    jobs a worker may take (`DUE` in `storage/jobs.rs`) leaves out a job whose
    owning user has a `users` row with `disabled_at` set. The user is read in
    the same statement as the job, inside the claim's write transaction (the
    idle poll's read uses the same predicate only as a hint, and the claim
    decides again). So a disable that commits before the claim is seen by it,
    and one that commits after the claim began to run cannot undo it: there is
    no read-then-claim window. A skipped job stays `queued` (or
    `retry_scheduled`) with the attempts it had, is not failed or released, and
    is due again, oldest first, once the user is enabled; the other users'
    jobs are claimed meanwhile. An actor with no `users` row is not disabled
    (store tests use such actors). **A job that is already running when its
    user is disabled is not stopped**: its worker keeps its lease, heartbeats
    and writes its result, which is the user's stored data. If that worker
    dies, the expired lease is not taken over until the user is enabled (the
    job stays `running`, and listing it shows that). There is no new state, no
    cancellation, and no schema change.
13. **Follow-up: a user purges their own trashed artifacts.** `POST
/v1/artifacts/{id}/purge` (decision 0029) accepts the administrator and a
    token issued to a **user other than the owner profile**
    (`RequirePurge`, `403` before the key, the identifier or the body is read,
    for anyone else). The purge is `ArtifactService::purge` unchanged: the base
    metadata revision, the request key (per user, item 7), the tombstone and
    the replay rules are those of 0029, and its owner check is the one of every
    other mutation, so another user's artifact (and the administrator's
    attempt on a user's artifact) answers the `404` a missing one does. A
    token issued to the **owner profile** is still refused: the owner has the
    bootstrap token, so an issued owner token is the agent credential of
    decision 0029, and no MCP tool purges. **The cost:** a non-owner user has
    no other credential, and the server cannot tell a person's token from an
    agent's, so an agent holding a user's token can purge that user's trashed
    artifacts, which cannot be undone. Content cleanup (`/v1/content/unreferenced`)
    stays administrator-only because it is global; a purge deletes the blobs no
    other revision file refers to in its own transaction (0029), so the cleanup
    only reclaims what was left by other means, and it is the administrator
    who runs it for every user's bytes.
14. **Follow-up: label uniqueness is documented where it is enforced.**
    `UserLabel` said "not unique"; the registry's `label` column is `UNIQUE`
    (exact, case-sensitive comparison; item 1 and the registry tests).

## Threat notes

- **A stolen user token** is that user's access and nothing else: it cannot list
  users, issue tokens, clean up content (administrator only), purge anything but
  its own trashed artifacts (item 13), or read another user's names. Revoke it; disable the user to close its open resources too.
- **Probing for another user's ids.** The answers for a real foreign name and an
  unknown name are byte-identical in the tests, including the order of checks:
  authentication, then administrator, then route names, then the body.
  A foreign name never produces a different status, so existence is not an
  oracle. Idempotent closes (`DELETE` of a view or a connection) answer success
  for a name that is not the caller's and close nothing of the holder's.
- **A disabled user is not distinguishable from an unknown token.** The
  response is the ordinary `401`, so a holder of a token cannot learn whether
  the token was revoked, the user disabled or the token never existed.
- **Resource exhaustion across users.** The bounds in decision 9 and the
  request budgets are per user, so one user (or agent loop) cannot starve
  another. The total memory is therefore bounded by users times the bounds (32
  users at most), not by the old single bound.
- **The administrator is also a single point.** The bootstrap token is the whole
  of user management. It is unchanged from decision 0023 and is not available to
  an agent through MCP.
- **Not addressed.** Preview and runtime-view links are bearer capabilities for
  their lifetime, as before: whoever holds a live link can load it. The content
  store is shared by digest, so two users who store the same bytes share a blob;
  no route reads by digest, `GET /v1/content/unreferenced` is administrator
  only, and a timing channel from that sharing is not analysed. A whole-database
  backup or restore (decision 0027) is an operator act on the data directory and
  contains every user's data.

## Not implemented

- Roles, sharing, per-object grants, a viewer or an editor, a user that reads or
  writes another user's artifact (S02). There is still no way to give anyone
  access to an artifact they do not own.
- OAuth, OpenID, external identity providers, passwords, login sessions, web
  sign-in as a user, or any identity other than the bearer token. The web
  workspace still signs in with the owner's token. No claim is made about
  identity federation.
- A web screen for users, and MCP tools for users or tokens (by decision).
- Cleaning up content as a user: content cleanup is global and
  administrator-only. A user can purge their own trashed artifacts (item 13),
  but the administrator's purge still reaches only the owner profile's
  artifacts, and there is no per-user cleanup.
- Purge through a token issued to the owner profile, and any MCP tool for
  purge (both stay refused, as in 0029).
- Deleting or renaming a user, erasing a user's data, transferring a token,
  per-user storage quotas, configurable per-user rate limits, an audit log,
  per-user backups or exports of the whole account.
- Frame identity (a runtime frame acts for the user whose workspace page asked),
  a multi-process or shared limiter and room store, and any statement about
  network isolation.
- Cancelling a disabled user's jobs: a job that is **running** when the user is
  disabled is carried out, and its result is the user's stored data (item 12).
  Queued jobs are not started while the user is disabled (item 12). A job that a
  delayed request enqueues after a disable and an enable is stored data of the
  user, not state the generation closes.
- Per-registration ownership: a refused late request closes everything the user
  holds, not only its own registration (item 11).

## Limits (not changed in this record)

- The per-user counts of rooms, preview tickets and fences, handoff inboxes and
  items, and runtime connections scan the whole table under its lock. That is
  bounded by 32 users times the bounds above, and is kept simple on purpose.
- The room counters of an actor (participant, tab and epoch numbers) are
  kept for the process's life and not pruned; there are at most 32 actors.

## Tests

`crates/artifree-server/tests/multi_user.rs` (HTTP and MCP isolation tables,
the route inventory check, user management and its administrator boundary,
token ownership, disabling and enabling, per-user budgets, the owner profile),
item 12 in `crates/artifree-providers/tests/jobs.rs` (a disabled user's job is
not claimed and uses no attempt, then is claimed after enabling; other users'
jobs are claimed in order; a running job completes; an expired lease is not
taken over; a claim waiting for the write lock sees a disable that commits
first), item 13 in `multi_user.rs` (the route table classifies the purge route
for the owner, Alice, Bob and an owner-profile agent token; a user's purge
rules; cleanup stays administrator-only),
`crates/artifree-providers/tests/users.rs` and `schema_upgrade.rs` (the registry,
its constraints, the state generation (raised by each change, never lowered, kept
across reopen), the per-actor feed, upgrades from every earlier schema
version, and an upgrade that keeps every change row, a purged artifact's and an
orphan's included, by value and by cursor), unit tests of each per-user bound and cascade in the server and core
crates, the races in `runtime_tests.rs` (a request that authenticated before a
disable, and before a disable and an enable, registers nothing: stream, link,
view, connection, inbox, join, tool call), `scripts/tests/user-management.test.mjs` (client and CLI).
