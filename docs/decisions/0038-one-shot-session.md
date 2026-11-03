# 0038: One-shot sessions

Date: 2026-11-04. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) (local storage),
[0022](0022-production-web-and-api-serving.md) (`serve`, listeners),
[0023](0023-single-owner-token-management.md) (tokens),
[0029](0029-retention-and-cleanup.md) (decision 10, what a later persisted
state must define), and [0036](0036-multi-user-identity.md) (users).
Roadmap row: O06. Independent Artifree design: the private evidence index lists
no original-product constraint on a temporary session for this row beyond the
roadmap sentence itself (temporary data, explicit session TTL, cleanup, no
inheritance, a tunnel TTL is separate). Nothing below is an observation of the
original product.

## Context

Every `serve` so far keeps its data: the operator names a data directory and
it outlives the process (0003, 0022). Someone who wants to try Artifree, let an
agent make something for an afternoon, or run a demo wants the opposite: the
data exists for one bounded session and is gone afterwards, with no tokens,
links, or jobs that a later run could pick up. That has to be a visibly
different mode, not a data directory that happens to be in `/tmp`, and it has to
clean up when the process is stopped by a signal, by its own deadline, and
after it was killed.

## Decisions

1. **What it is.** `artifree-server serve --one-shot-ttl-seconds <n>` runs the
   same server on a new temporary data directory that is deleted when the
   server stops. It is chosen by the explicit lifetime flag only: there is no
   environment variable, no default lifetime, and no way to reach it by
   leaving out `--data-dir`. `--data-dir` and `--one-shot-ttl-seconds` exclude
   each other, `--session-root` is accepted only with the lifetime flag, and a
   `serve` with neither is refused with a message that names both. All of this
   is checked before anything is created. Everything else about serving
   (listeners, origins, previews, limits, the HTTP and MCP surfaces) is the
   persistent server unchanged; the storage and permission paths are the same
   code (`ArtifactService` over the same SQLite store).
2. **Where the data lives.** A session owns one directory, named
   `artifree-session-<16 hex digits>`, under the session root: `--session-root`,
   else the system temporary directory. The root is created private if it does
   not exist. The directory is created with mode 700 (Unix; a Windows
   directory inherits its parent's access control, as for `--data-dir`) and
   fails if the name exists. It holds a marker file (decision 6) and a `data`
   directory, also 700, which is the data directory handed to the unchanged
   startup (database, WAL, access token); the existing check that refuses a
   directory open to other users runs on it.
3. **The session lifetime.** The lifetime is a whole number of seconds from 1 to
   604800 (seven days), counted from the start of `serve` in the process, before
   storage is opened. Startup (creating the directory, the stale sweep, opening
   the database, binding the listeners) is therefore part of the lifetime, and
   `session_ttl_seconds` is the total lifetime, not the time after readiness. When it ends the server stops accepting connections, closes its
   change streams, gives open connections five seconds
   (`SESSION_DRAIN_GRACE`) and then abandons them, and deletes the session
   directory. A client that holds a connection open therefore cannot keep the
   data alive. The lifetime is not extended by activity and cannot be changed
   while running. Tests use the real mechanism with a 2 to 3 second lifetime
   and wait on the process exit, not on a fixed delay.
4. **Cleanup on normal and signalled exit.** The session directory is removed
   after the runtime, the database connections, and the listeners are gone, on
   all of these paths: the lifetime ends, SIGINT, SIGTERM (Ctrl+C on other
   platforms), a startup failure after the directory was created, and a panic
   that unwinds (`Drop`). A removal that fails is an error of the process (exit
   code not zero, message on stderr) that says the next session will remove
   the directory. The signal handlers are installed before the readiness line is
   printed: a supervisor that signals the server as soon as it reads that line
   would otherwise kill it with the default action and skip the cleanup (this
   was a real failure found by the process tests). Persistent mode gets the
   same ordering, which changes nothing it promised.
5. **Cleanup after an abnormal exit.** SIGKILL, power loss, an OOM kill and a
   crash leave the directory behind. This is handled by the smallest mechanism
   that is correct, a sweep at the next one-shot start (not a daemon, not a
   timer, not a persistent mode feature): the new session removes every
   sibling in the session root for which all of these hold: the name starts
   with `artifree-session-`; it is a real directory, not a symbolic link; it
   is owned by the same user as the new session's directory (Unix); it has a
   marker that is a regular file in the current format; and the marker's owner
   process no longer exists. A directory missing any of these is left alone,
   including one with no marker. The report (`removed`, `failed`) is in the
   readiness line.
6. **The marker and the owner check.** The marker `.artifree-session` is
   `artifree-session 1`, `pid <n>`, and, where `/proc` exists, `start <ticks>`
   (the process start time). The owner has ended when `/proc/<pid>/stat` is
   missing, shows a zombie (state `Z` or `X`), or has a different start time
   (the process id was reused). **Where `/proc` does not exist (macOS,
   Windows) nothing can be shown to have ended, so the sweep removes
   nothing there** and a killed session's directory stays until removed by
   hand. This errs toward keeping data: a wrongly kept directory costs disk
   space; a wrongly swept one would delete a running session's database.
   Sessions that share a session root must share a process id namespace;
   across namespaces (two containers on one volume) the process id means
   different things and the sweep must not be used, so give each its own
   `--session-root`.
7. **No inheritance.** A new session always starts empty. It has a new
   directory, a new database, a new random owner access token, no tokens
   issued by an earlier session, no artifacts, no request keys, no jobs, no
   users, and no preview, runtime, or room state (that state is in memory in
   any case, 0003 and 0034). The previous session's bearer secrets are not
   known to the new database, so they do not authenticate (`401`), a job id or
   preview address of an earlier session is the `404` an unknown name gets, and
   the new session's content origin does not serve an old preview address.
   There is no flag to keep, copy, or import a session's data. Backup (0027)
   works on a running one-shot session like on any data directory, which is
   the way to keep anything: a backup file that the operator writes outside the
   session root is the operator's own copy and is not touched by the cleanup.
   A one-shot session adds no digest-bearing state outside its own database,
   so the reference rule of the content cleanup (0029, decision 10) does not
   change.
8. **Telling the modes apart.** The readiness line has `mode`: `persistent` or
   `one_shot`, and `data_dir` in both. Only a one-shot line has
   `session_ttl_seconds`, `stale_sessions_removed`, and `stale_sessions_failed`.
   A path in the readiness line must be valid UTF-8: otherwise (for example a
   `TMPDIR` of arbitrary bytes, which is the default session root) the server
   refuses to serve with an error that names the problem, exits with code 1
   before accepting a connection, and removes the session directory; this also
   replaces what was a panic for a non-UTF-8 `--data-dir` in persistent mode. A one-shot server also prints one line on stderr when it is ready, so a
   person at a terminal is told that nothing is kept. `--help` and the README
   describe the two modes separately. **No HTTP response, no MCP tool, and no
   contract changed**: the API cannot tell a client which mode it talks to,
   `/healthz` is unchanged, and the generated contracts and the contract
   version are the same. (A field in an API response would be a wire change
   and is not made here.)
9. **A tunnel lifetime is another thing.** The data lifetime above governs how
   long data exists. A tunnel or public link lifetime, how long a remote
   address stays reachable, is a different setting with a different cost to
   expire. Artifree has no tunnel, so no tunnel lifetime is implemented and
   none is implied: `--one-shot-ttl-seconds` does not publish, expose, or
   expire any address, and an operator who puts a tunnel in front of a one-shot
   session (0024) sets that tunnel's own lifetime. Ending a tunnel does not
   end the data, and the end of the data ends whatever the tunnel pointed at.

## Limits

Implemented: the mode, the flags, the private temporary directory, the
lifetime, cleanup on lifetime end, SIGINT, SIGTERM, startup failure and
unwinding, the sweep (Linux), and the process tests above.

Not implemented:

- **Sweeping on macOS and Windows.** A killed session there leaves its
  directory (decision 6).
- **A stale directory is only removed when another one-shot session starts.**
  No timer, no `artifree-server sweep` command, no systemd or launchd unit,
  and no cleanup when a persistent server starts.
- **SIGHUP, SIGQUIT and Windows console-close events** are not handled; they
  end the process like a kill and leave the directory for the sweep.
- **A crash between creating the directory and writing its marker** leaves a
  directory the sweep ignores for lack of a marker. Both steps are one
  function and microseconds apart.
- **Secure erasure.** The directory is unlinked; SQLite pages and file
  contents are not overwritten, and `secure_delete` (0029) does not apply to
  files that are removed. A file system snapshot, journal, or swap holds what
  it holds.
- **A worker for one-shot data.** The persistent job worker (0030) is a
  separate process on a data directory; it can be pointed at the `data_dir`
  of the readiness line, but it is not started, supervised, or stopped with the
  session, and a worker that outlives the session works on a deleted
  directory.
- **An API or UI indicator of the mode**, a countdown, extending the lifetime,
  and a different default lifetime.
- **A quota or size limit** for a session's data beyond the limits of any
  server.
- **Tunnel lifetime** (decision 9) and any link or address expiry other than
  the existing preview link lifetime.
- **Windows**: the code was not built or run there. The process tests are
  Unix only, and the mode, the Ctrl+C path, and the cleanup are unverified on
  Windows.
