# 0039: One-shot session mode

Date: 2026-11-04. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) (the server and data directory),
[0023](0023-single-owner-token-management.md) (the owner token),
[0029](0029-retention-and-cleanup.md) (what a persistent data directory keeps),
and [0036](0036-multi-user-identity.md). Roadmap row: O06. Independent Artifree
design: the private evidence index lists no original-product constraint on a
temporary session for this row. Nothing below is an observation of the
original product.

## Context

`artifree-server serve` always needs `--data-dir` and keeps everything in it
for ever. A person who wants to try the product, or an agent that wants a
scratch workspace, has to pick a directory and remember to delete it, and a
leftover directory holds a database and an owner token. O06 asks for a mode
whose storage is temporary by construction.

Earlier attempts at this row kept failing review for one reason: each added
another place in the startup sequence where a stop request, a failure, or an
unknown fact about another process was handled as "carry on" or "safe to
delete". This decision therefore fixes the structure first.

## Decisions

### 1. The mode

`serve --one-shot-ttl-seconds <n>` runs the normal server on a **new private
temporary data directory** instead of `--data-dir`.

- `<n>` is ASCII digits only, 1 to 604800 (seven days). `+5`, ` 5`, `5s`, `0`
  and larger values are refused. There is **no default lifetime**: the flag is
  how a person asks for this mode.
- `--session-root <dir>` (one-shot only) names the parent of the session
  directory; the default is the system temporary directory. It must exist.
- `--data-dir` together with `--one-shot-ttl-seconds`, and `--session-root`
  without it, are refused while the arguments are read, before anything is
  created.
- The directory is `artifree-session-<32 hex>` under the root, mode 0700 on
  Unix (the platform default access elsewhere, as for `--data-dir`). It holds
  the database, the owner token (mode 0600 on Unix), and a marker file
  `artifree-session` (decision 5).
- A session starts empty with a **new owner token**. Nothing from an earlier
  session is read: an old token is `401`, an old artifact, job, or preview
  address is `404`, because the new database has never heard of them. The mode
  has no import and no export; use persistent mode and `backup` for data that
  must stay.
- Persistent mode is unchanged: same flags, same storage, same behavior,
  including that a failed write of the readiness line is only logged and the
  server keeps serving. The readiness line of both modes gains `mode`
  (`"persistent"` or `"one-shot"`) and `data_dir`; a one-shot line also has
  `session_ttl_seconds`.

### 2. Lifetime and end of a session

The lifetime is counted from process start, so startup time is part of it. The
session ends, and its directory is removed, on:

- TTL expiry;
- SIGINT, SIGTERM and SIGHUP (Unix) or Ctrl+C (Windows). SIGHUP ends a session
  **even under `nohup`**, because the handler replaces an ignored disposition.
  The signal list shown to a person is generated from one platform constant;
- a startup failure, a failed write of the readiness line, a listener failure,
  and a panic unwinding out of the session (a guard removes the directory).

A normal end (TTL or signal, before or after readiness) exits 0. A failure
exits 1 after cleanup. After a stop request the servers drain for at most 5
seconds (a stalled client does not keep the data alive); then the runtime is
stopped and its tasks dropped, and the directory is removed only **after no
database holder is left**: the state's identity store is held by the state and
by every blocking task a request started (each clones it or the shared state),
so its strong count reaching zero means that no blocking SQLite task of the
session can still be running. The wait for that is bounded by 20 seconds
(SQLite's busy timeout is 10 s for the artifact store, 5 s for the identity
store), says so on stderr when it has to wait, and a holder still running at
the bound is an explicit error (exit 1) whether or not the removal then
succeeds. A failed removal keeps the path so that the guard's drop tries once
more. Removal therefore also works where open files block it (Windows).

`SIGKILL`, power loss, and an uncatchable termination leave the directory; that
is what the sweep (decision 4) is for. This is **not secure erase**: removal
unlinks files; blocks stay on the device until reused. There is no size quota.

### 3. Startup is one ordered list of stages (structural rule)

All one-shot startup runs as the named stages `create-dir`, `sweep`,
`open-database`, `open-token-store`, `write-token`, `bind-api`, `bind-content`,
`bind-loopback` (only when a loopback address is configured or derived),
`build-state` and `readiness`, executed by **one function** (`run_startup`).
The state is built after the binds because its settings (allowed hosts, preview
origin) need the addresses that port 0 chose, as in persistent mode. Before
every stage it asks whether a stop was requested; if so it skips that stage and
all later ones. Stages are synchronous (listeners are bound with `std` and
handed to Tokio), so there is no await between a stage and the next check. The
stage bodies are private methods reachable only through that function.

A stop request is a latch (set by a signal, by the TTL timer, or by a test).
Signal handlers and the timer are installed **before** the directory is
created, so a signal that arrives earlier is honored at the first checkpoint.
A stop at any point skips the remaining stages, drops everything created, removes
the directory, and exits 0. The sweep checks the latch between directory
entries too.

A debug-only seam (`ARTIFREE_TEST_HOLD_AT=<stage>` or `sweep-entry`, read
only when `debug_assertions` is on, absent from release builds) holds the
process before that stage, or before each sweep entry, until a stop is
requested (at most a minute). It exists so tests can stop a real process at
an exact point; the tests that use it skip, saying so, in a release build.

### 4. Sweep of dead sessions (fail closed)

At the next one-shot start, directories of sessions that were killed may be
removed. A directory is removed **only when every condition holds**; any
doubt keeps it:

1. its name starts with `artifree-session-`;
2. it is a real directory (`symlink_metadata`), not a symbolic link;
3. its owner is our own user, compared with our own new session directory (if
   that cannot be read, nothing is swept);
4. it holds a regular file `artifree-session` in exactly the current format:
   the line `artifree-session 1`, the line `pid N`, an optional line
   `start T`, and nothing else (an extra, duplicate or blank trailing line means
   "not ours"); the read is size-bounded;
5. the owning process is **proven gone**. The lookup has three answers:
   running, gone, unknown. Gone means only: the process entry does not exist
   (`NotFound`), its state is zombie or dead, or its recorded start time
   differs from the marker's (the pid was reused). A permission error, any other
   I/O error, an unparsable entry, a process table that is missing or does not
   contain our own process, or a platform without `/proc` is **unknown, and
   unknown keeps the directory**.

The sweep is effective **on Linux only**, and only for processes in the pid
namespace of the process table it reads (the table root is injectable for
tests). A crash between creating a directory and writing its marker is never
swept. It reports counters (removed, kept as running, kept as unknown, kept as
not ours, failed). It is not a security boundary: it removes data of this user's
own dead sessions, nothing else.

### 5. Readiness line

The readiness line is built from a `Serialize` struct and written with
`writeln!` on a locked stdout; a data or token path that is not valid UTF-8, or a
failed write, is an **error in one-shot mode** (exit 1, directory removed). It
never uses `json!` with a path. In persistent mode a failure is logged on
stderr and serving continues, as before.

A person is told on stderr that nothing is kept, which signals and which
lifetime end the session, and that a tunnel lifetime is a different thing.

### 6. Not the tunnel lifetime

A tunnel or public-link lifetime would limit how long a remote address works.
That concept is **not implemented** (no tunnel exists; see 0024) and this TTL is
only the lifetime of the data and the process. The two must not be conflated
when a tunnel is added.

## Not implemented

- A second Ctrl+C (or SIGINT) during shutdown does not force the process to
  quit: the handler is registered for the process and swallows it. SIGTERM
  and SIGKILL still do; SIGKILL leaves the directory for a later sweep.
- The sweep assumes a shared pid namespace with the dead session (decision 4).

- Secure erase, a size quota, removing a session on SIGKILL or power loss other
  than by a later sweep, a sweep on macOS or Windows, a sweep across pid
  namespaces or containers, a sweep of a directory whose marker was never
  written.
- Extending a running session, a default lifetime, an environment variable for
  the TTL, a multi-session manager, exporting a session.
- A tunnel TTL.
- Windows signals other than Ctrl+C (console close, logoff, shutdown).
- Release-build coverage of the stop-at-stage tests (the seam is compiled out).
- A test that the runtime and database are dropped before the directory is
  removed: the order is structural (the guard is declared before the runtime),
  and only Windows, where open files block removal, would show a mistake; CI
  runs on Windows, but no test there stops a session while a request is open.
- A test of the same-user check of the sweep (a second user's directory cannot
  be made by the test); the unreadable-own-directory case that makes it sweep
  nothing is tested.

## Verification

`one_shot` unit tests drive the stage runner with a fake stage list: for each
stage k a stop latched before k means stages k.. never run. Real-process tests
stop the server during the sweep, between the two binds, right after
readiness, at the TTL, and by each signal; check modes and the marker; show a
stalled client does not keep the data; show a live owner is not swept while a
dead owner is; show look-alike directories and unknown owner states are kept;
show no state carries over; and show persistent mode keeps serving after a
failed readiness write.
