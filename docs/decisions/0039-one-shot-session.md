# 0039: One-shot session

Date: 2026-11-04. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) (local storage),
[0022](0022-production-web-and-api-serving.md) (the production server),
[0029](0029-retention-and-cleanup.md) (retention), and
[0036](0036-multi-user-identity.md) (identity). Roadmap row: O06. Independent
Artifree design: the private evidence index lists no original-product
constraint on temporary sessions; nothing below is an observation of the
original product.

## Context

Until now `artifree-server serve` always needs `--data-dir`, and everything in
it lives until an operator deletes it (0029 keeps even request keys for the life
of the database). Some uses want the opposite: try Artifree, let an agent build
something, look at it, and leave nothing behind. That needs a session whose data
is created for one run, has an explicit lifetime, and is deleted when the run
ends however it ends. It must not blur into the persistent mode: a person must
be able to tell from the command line, the readiness line, and stderr which
mode they are in.

**Implemented:** everything under "Decisions" (code in
`crates/artifree-server/src/{one_shot,session,sweep,process_table}.rs`).
**Not implemented:** see the last section.

## Decisions

1. **A mode, not a new server.** `serve --one-shot-ttl-seconds <n>` runs the
   normal server on a new temporary data directory. `n` is a whole number from 1
   to 604800 (seven days). There is **no default lifetime**: the mode exists
   only when asked for. `--session-root <dir>` (default: the system temporary
   directory, `TMPDIR` on Unix) is where the directory is created; it must
   already exist, and it only applies to this mode.
2. **Refused combinations create nothing.** `--data-dir` with
   `--one-shot-ttl-seconds`, `--session-root` without `--one-shot-ttl-seconds`,
   a value outside the range, and a `serve` with neither `--data-dir` nor
   `--one-shot-ttl-seconds` are errors raised while reading the options, before
   any file or directory exists. Persistent mode (`--data-dir`) is unchanged.
3. **A private, new directory.** The session directory is named
   `artifree-session-<16 hex digits>` under the session root and created with
   mode 0700 on Unix by `mkdir` (it fails if the name exists, so an existing
   directory or a planted symbolic link is never adopted); the mode is checked
   again after creation. Inside, `serve` uses it as its data directory exactly
   as in persistent mode: database, WAL, `access-token` (mode 0600). On other
   platforms the directory inherits its parent's access control, as 0003 states
   for the persistent directory.
4. **Never resumed.** Every session starts empty with a new owner token, new
   database, and new in-memory previews, grants, and rooms. A token of an earlier
   session answers `401`; the job or preview address of an earlier session
   answers `404` (the artifact, the job, and the preview ticket do not exist).
   Nothing refers to the old directory.
5. **Lifetime.** The TTL is the total lifetime of the process, counted from the
   start of the `serve` command (`Instant` taken before anything is created).
   **Time spent starting is part of it**: a one-second TTL on a slow disk may
   end the session before it is ready, which ends it cleanly, without a readiness
   line. When it expires the server stops accepting work and shuts down as for a
   signal (item 6). The TTL is a deadline for the process, not a quota on
   requests.
6. **Cleanup in every exit path.** The directory is removed on TTL expiry,
   SIGINT, SIGTERM, SIGHUP (the signal of a closed terminal), on Unix, or
   Ctrl+C on Windows, which handles nothing else (the notice and `--help` take
   the list from one constant, so each platform's text is true), a startup failure, a failed readiness
   callback, a listener failure, and a panic that unwinds. Order matters and is
   fixed: (a) the signal handlers are installed **before** the directory is
   created, by building the runtime first; a signal that arrives later is
   recorded, and a stop is looked for before every step that creates or changes
   something: the directory, each entry of the sweep, the database, the token
   store, the token, each listener, the application state, and the readiness
   line. A step that has started is finished; none starts after a latched stop,
   and a stop during start-up never opens or migrates the database after it; (b) the listeners stop and open connections get a bounded
   drain of **5 seconds**; a client that stalls cannot keep the data alive past
   it; (c) the runtime is shut down (with a short bound for blocking work), which
   drops every connection and with it the database handles; (d) only then is the
   directory removed. A guard owns the removal so that unwinding performs (c)
   before (d) as well: it is declared before the runtime and so dropped after it.
   The result is `Ok` for a TTL or a signal, and an error with exit code 1 when
   startup failed, the readiness line could not be written, or the directory
   could not be removed (the message names it, because data may remain).
   `SIGKILL`, power loss, and the kernel's OOM killer cannot run any code; item 8
   covers them.
7. **Readiness line and stderr.** The line gains `mode` (`one_shot` or
   `persistent`) and `data_dir`; a one-shot line also has `session_ttl_seconds`
   and `sweep` (item 9). It is built from a `Serialize` struct of `&str`
   fields, never `serde_json::json!` with a path, and written with `writeln!` to
   the locked standard output; a path that is not valid UTF-8 (for example a
   `TMPDIR` with invalid bytes) or a failed write (a closed pipe, a full device)
   is an error, exit code 1, and the cleanup of item 6. A one-shot session
   writes to stderr that nothing is kept, which directory is deleted, and when.
   The line stays one JSON object; stdout holds nothing else. Persistent mode
   builds its line the same fallible way (its bytes only gain the two fields)
   but keeps its earlier behavior when the line cannot be written: the server
   logs the problem on stderr and keeps serving, where a one-shot session would
   end.
8. **Crash cleanup: a later one-shot start may sweep.** A killed session leaves
   its directory. The next one-shot start sweeps the session root (never the
   persistent mode, never an unrelated start). Because deleting a live
   session's data is the worst failure, **every doubt keeps the directory**. An
   entry is removed only when all of the following hold:
   1. its name is `artifree-session-` plus 16 lowercase hexadecimal digits;
   2. `symlink_metadata` says it is a real directory (not a symbolic link);
   3. it is owned by the same Unix user as the directory this process just
      created (if that user cannot be read, or the platform has no such notion,
      nothing is swept);
   4. it holds a regular file `.artifree-session` (not a link, same owner, at
      most 256 bytes) in exactly the current format;
   5. the owner named in that file is **proven** gone.

   The marker is `artifree-session v1`, `pid N`, and optionally `start T`, each
   on its own line, nothing else; one final newline is allowed. A third line that
   repeats `pid`, a blank line, carriage returns, a number with a sign or leading
   zero, `pid 0`, or any extra line makes the file not ours, and the directory is
   kept. `T` is the owner's start time in clock ticks since boot, field 22 of
   `/proc/<pid>/stat`, written when it can be read.

9. **Proof that an owner is gone.** The process table answers three ways:
   _running_, _gone_, or _unknown_, and only _gone_ permits removal. _Gone_ is a
   definite "no such file" for `/proc/<pid>/stat`, a state of zombie or dead, or
   a start time different from the marker's (the pid was reused). Permission
   denied, any other I/O error, a `stat` that does not parse (the process name
   is skipped up to the last `)`), a `stat` of another pid, a table that is
   not there, and every platform other than Linux are _unknown_. Before any
   answer the table must show this very process as running, so that an
   unmounted `/proc` is never read as "everything is gone". The root of the
   table is injectable, so tests build a proc-shaped directory and produce each
   failure kind for real. The sweep reports `performed`, `removed`, `kept`, and
   `failed` in the readiness line; a removal error is counted and does not stop
   the others.
10. **A debug-build test seam.** To test a signal during startup
    deterministically, a build with `debug_assertions` (so `cargo test`, never a
    release package) reads `ARTIFREE_TEST_HOLD_STARTUP=<stage>=<file>` where
    `<stage>` is `before-directory`, `after-directory`, `after-sweep`, or
    `after-database`:
    on reaching that checkpoint the server creates `<file>` and waits until a
    stop is requested. A release build does not contain the code. It is not in
    `ServeEnv` (0028) because it is not an operator setting. A timing-based test
    could pass or fail on a fast or slow machine without testing the property.
11. **Deleting the directory is what ends a token.** The token file and the
    token tables are inside it and there is no other copy. A client that
    remembers the old token learns nothing: the next session has a different
    token and a different database, and answers the old one with `401`.
12. **SIGHUP ends a session even under `nohup`.** Registering a handler for
    SIGHUP replaces an inherited "ignore", so `nohup` does not protect a
    one-shot session. It is a temporary mode by design; use `--data-dir` for a
    server that must outlive its terminal.

## Known limits (stated, not hidden)

- **No sweep on macOS or Windows.** Their process tables are not read; the
  sweep reports `performed: false`, and a killed session there leaves its
  directory until a person deletes it.
- **The sweep happens only at the next one-shot start.** There is no daemon, no
  timer, and no sweep at boot.
- **Sessions sharing a session root must share a PID namespace.** A marker
  from another container looks like a pid that is not running here. With
  different namespaces, give each its own `--session-root`.
- **No secure erase.** Removal unlinks files; blocks may stay on the device,
  snapshots, backups, or swap. Use an encrypted volume for stronger needs.
- **No size quota.** A session can fill the disk of its session root until the
  TTL; the existing request limits (0005) are per request.
- **A crash between creating the directory and finishing its marker leaves a
  directory that is never swept**, because a directory without a complete
  marker is kept.
- **The drain bound is not a guarantee against a stuck blocking task**; after
  the runtime's short shutdown bound the directory is removed anyway, which on
  Unix is allowed with open files.
- **Another user's process cannot be told from an unreadable one**, so sessions
  of another Unix user are never swept.

## Not implemented

- **A tunnel or proxy TTL.** It is a separate concept (what a public address
  lives for, and who may open it) and it does not exist. The data TTL of this
  decision does not close a tunnel, and closing a tunnel does not delete data.
- Persistent mode still installs its signal handlers when it starts serving
  (unchanged since 0003), so a signal in the instant between its readiness line
  and the first request ends it by the default action. A one-shot session
  installs them before it creates anything.
- Verification on Windows or macOS: the one-shot code compiles there by
  construction (`cfg` gates, no sweep) but was only built and tested on Linux.
- Resuming, extending, or listing sessions; a TTL that renews on activity; a
  session quota; export of a session before deletion; a Windows or macOS sweep;
  a web or MCP surface for sessions; a one-shot container recipe.
- A session of the `worker` command: `artifree-server worker run` takes a data
  directory and can run on a session's `data_dir` from the readiness line, but
  it ends with its own signals and is not tied to the session's lifetime.

## Verification

`crates/artifree-server/tests/one_shot_session/unix.rs` (real child processes: modes,
SIGTERM and SIGINT cleanup, TTL expiry, a stalled client, signals at each start
stage and right after the readiness line, no carry-over of tokens, jobs, and
previews, a live owner that is not swept while a dead one is, look-alike
directories, contradictory flags, a non-UTF-8 `TMPDIR`, a full standard output,
persistent mode unchanged) and unit tests in the four modules (marker format,
each process-table failure kind, each sweep guard, unwinding). Each guarded
property was also checked by breaking the guard and watching its test fail; the
list is in the pull request.
