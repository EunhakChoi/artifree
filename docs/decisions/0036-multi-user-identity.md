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
   `created_at` and `disabled_at`. The owner is user number 1 with the id it
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
   and runtime view fences, runtime connections (and so their grants and
   handoff offers), room participants, and registered handoff inboxes. Nothing
   stored is touched: artifacts, revisions, data, jobs and the user's tokens
   stay, and the tokens are not revoked. **Enabling lets the tokens that are not
   revoked authenticate again; what disabling closed stays closed**, so a client
   opens a new connection, stream or link. Token rotation and per-token
   revocation work as before, and a revoked token's cascade (streams, tickets,
   connections) is unchanged.
5. **Registering after the disable is closed by confirming after.** A request
   that authenticated before a disable can register something that outlives it
   (a stream, a preview link, a runtime view) after the disable's cleanup ran.
   Those paths register first and then call `ensure_enabled`, which reads the
   user and, if it is disabled or gone, runs the cleanup again and answers `401`.
   Either the request sees the disabled user and removes its own registration, or
   the disable's cleanup comes later and removes it. Runtime connections close
   before rooms are emptied, and a room call checks its connection under a lock,
   so no participant outlives its connection.
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
   commit is neither delivered nor visible as a gap in numbering.
9. **Bounds that were global are per user.** Rooms with a participant
   (`max_rooms`), the numbers rooms hand out (participant, tab, epoch), runtime
   connections (4096), preview tickets, view fences and pending creations (4096
   each), handoff inboxes (128) and items (512). The request limiter, the three
   request budgets and the open-stream cap were already keyed by actor; they stay.
   Each is tested for "one user at the bound, another unaffected".
10. **Schema 10 and contract 0.14.0.** One transaction: `users` (with the owner
    row, a `CHECK` that the owner is never disabled, and `CHECK`s on the id
    spelling), `tokens` rebuilt with `actor`, `actor_feeds`, `changes` rebuilt
    with `actor`/`actor_seq`. The feed head is carried over. The structural check
    that compares a database with a fresh one, and the restore path of decision
    0027, include the new tables. `TokenInfo.user_id`,
    `CreateTokenRequest.user_id`, `UserInfo`, `CreateUserRequest`, `UserList`
    are new in the generated contract.

## Threat notes

- **A stolen user token** is that user's access and nothing else: it cannot list
  users, issue tokens, purge or clean up (administrator only), or read another
  user's names. Revoke it; disable the user to close its open resources too.
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
- Purging or cleaning up as a user: both are administrator-only, so a user's
  trashed artifacts cannot be purged by that user, and the administrator's purge
  reaches only the owner profile's artifacts.
- Deleting or renaming a user, erasing a user's data, transferring a token,
  per-user storage quotas, configurable per-user rate limits, an audit log,
  per-user backups or exports of the whole account.
- Frame identity (a runtime frame acts for the user whose workspace page asked),
  a multi-process or shared limiter and room store, and any statement about
  network isolation.

## Tests

`crates/artifree-server/tests/multi_user.rs` (HTTP and MCP isolation tables,
the route inventory check, user management and its administrator boundary,
token ownership, disabling and enabling, per-user budgets, the owner profile),
`crates/artifree-providers/tests/users.rs` and `schema_upgrade.rs` (the registry,
its constraints, the per-actor feed, and upgrades from every earlier schema
version), unit tests of each per-user bound and cascade in the server and core
crates, `scripts/tests/user-management.test.mjs` (client and CLI).
