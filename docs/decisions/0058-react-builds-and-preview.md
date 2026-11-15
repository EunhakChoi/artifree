# 0058: React builds and their preview

Date: 2026-11-14. Status: **accepted; R03a (the package rules), R03ba
(the profile table), R03bb (the provider's verdicts), R03bca (the
versioned, packaged and protected tool root), R03bcba (the lock list, the
packaged bundler and its measurement), R03bcbb (the `react-v1` profile in
the build with `enforcement-fixture`), R03ca (the store of builds and
diagnostics), R03cb (the worker's composition, diagnostics and failures),
R03cc (build tickets, the `/b/` route and the head status, below the
wire), R03cda (the execution readiness, schema 16), R03cdb (the
claim deadline), R03da (the public kind, contract 0.30.0) and R03db
(build tickets and the head status on the wire, contract 0.31.0), R03ea
(the build in the Jobs tab) and R03eb (the build preview) are implemented,
R03f is not**.
Roadmap: R03.

> **Note, 2026-11-14 (slices).** R03b is split into R03ba, R03bb and R03bc,
> and rule 5 moves to R03f. A release build compiles no attempt, worker port,
> collection, staging, registry or lock today (0057 rule 2 and its R02bb
> note), so letting it know `react-v1` is a change of its own, and it lands
> with the supported-host statement it makes true. Rules 5 and 22 are
> reworded to match; the other rules are unchanged.

> **Note, 2026-11-14 (R03ba).** The provider holds one table of execution
> profiles. Each entry fixes its store name, its inspection identity and
> manifest, its payload, the payload's arguments and whole environment, and
> its filter; the stub is given `stub` and the identity. The bootstrap, the launcher's argument
> list, the attempt plan, the output binding and the worker's adapter look a
> profile up by its exact identity or name instead of naming the fixture.
> Capacity is taken before the claim (0042 rule 5), so no profile is known
> when it is taken: `hold` keeps the digest of each filter that builds, and
> a launch whose filter did not build is refused alone. The fixture is the
> only entry, and the host inspection keeps the fixture's inspection profile.

> **Note, 2026-11-14 (R03bb).** The execution port tells a failed run apart:
> `Exited` for a non-zero exit, `FilterKilled` for the filter's kill,
> `Limit` with the wall time, the memory or the log bound, and `Failed` for
> any other failed verdict or refused output. The launcher reports a payload
> that a signal ended as 128 plus the signal, so an exit status of 159 reads
> as the filter's `SIGSYS`; a payload that exits with 159 by itself reads the
> same way, and since both are `build_failed`, only the fixed diagnostic
> differs. A stub that a signal ended itself, or a status of 0 with no
> output, is `Failed`. The stub's status is that of the whole trusted chain:
> a failure of the bootstrap after the release reads like the payload's own
> exit, as it did before this slice, and R03cb must keep that in mind when it
> names these failures. The launch hands the stub's stderr to the worker on
> every path on which the payload started, within the 1 MiB bound it shares
> with stdout, and none when the payload did not start, so the bootstrap's
> own diagnostics never pass for the payload's. The worker
> prints only its length beside the provider's word, and still fails each such
> job as `internal` until R03cb.

> **Note, 2026-11-14 (R03bc split).** R03bc is split into R03bca and R03bcb.
> R03bca changes how every root is assembled and admitted, and the fixture
> already exercises all of it. R03bcb adds the bundler, its packages and its
> filter on top. Rule 19 leaves open which version the root's path carries.
> It is the server's package version: the release workflow checks that
> version against the tag without its `v`, so the root of server 0.1.0 is
> `/opt/artifree/tools/0.1.0`. Only a release names a root that an operator
> keeps. A development build shares its version with others, so it is no
> supported host, and its operator removes the old root first. The
> AppArmor launcher profile carries the same version in its name, so the
> roots of two versions can stand side by side during an upgrade. The
> script only creates a root. The verbs of `prepare.sh` that rewrite the
> job's root in place (its scenario and its tampering) belong to the test
> harness, not to the script. The provider admits a root only if the root,
> each of its entries and each ancestor directory is owned by root, has no
> group or other write bit and no set-user-ID or set-group-ID bit. An
> ancestor may be world-writable when its sticky bit is set, as `/tmp` is.
> Rules 4, 22 and 23 are reworded to match, and 0056 gets a note on its
> fixed path.

> **Note, 2026-11-14 (R03bca).** `scripts/tools/package-root.sh SERVER`
> assembles `/opt/artifree/tools/<version>` from the version that
> `SERVER --version` names. It installs the pinned launcher and its licence,
> the stripped server as the bootstrap, their libraries and licences, and
> the manifest, and then loads the profile `artifree-launcher-<version>`.
> It refuses a root that exists. `prepare.sh` sources it, checks that the
> job's server names the workspace version, and adds the fixture, the
> payload, the probes and the job's files before the manifest. The
> provider's `TOOL_ROOT` is the same path, built from its package version.
> Admission checks the root's ancestors after the launcher and the fixture,
> and checks every entry during the manifest walk; a failure of either
> refuses as `ToolRootUnprotected`.

> **Note, 2026-11-14 (launcher libraries).** Rule 19 said the script
> packages "the libraries they load", meaning the launcher's and the
> bootstrap's. The R03bca note above repeats this ("their libraries"), but
> the script copies only the bootstrap's. It fits only the bootstrap. The
> launcher runs on the host, outside the sandbox, through the host's loader,
> and that loader needs the C library of its own glibc build. Pointing the
> launcher at copies in the root (a library path or an `$ORIGIN` run path)
> would still use the host's loader with a pinned C library, so a host glibc
> update could break the pair. Starting the launcher through a loader copied
> into the root would also break it. The AppArmor profile would then have to
> attach to that loader's path, and every program it loads would get user
> namespaces. So the root holds the libraries of the programs that run
> inside the sandbox. Today that is the bootstrap, and in the enforcement
> job the fixture and the payload. The bundler's libraries come with R03bcb.
> The fixture also runs on the host for the readiness probe, with the host's
> libraries; its copies serve its runs inside the sandbox. The launcher's
> loader and libraries are the host's, like the kernel. The supported host
> (Ubuntu 24.04 with its security updates) keeps them current. The launcher
> binary stays pinned in the root, and the script refuses a newer
> Bubblewrap. Nothing at admission notices when one of those host libraries
> changes; that drift is an accepted limit of the supported host, not a
> checked property. 0042 rule 3 lists the loader and libraries "required by
> that profile", which means the profile's runtime, so the rule is
> unchanged; 0042 gets a note. A statically linked launcher, which would
> bring its libraries into the root, would be a new decision. Rule 19 is
> reworded to match.

> **Note, 2026-11-14 (R03bcb split).** R03bcb is split into R03bcba and
> R03bcbb. The filter of rule 3 is measured from the bundler, and the bundler
> has to be in a root before it can be measured. The earlier notes' "R03bcb"
> now means these two slices. The bundler is statically linked, so it brings
> no libraries.
>
> R03bcba adds the following:
>
> - The lock list.
> - The packaging script's download, check and unpacking of the bundler
>   and the packages.
> - A test template under rule 7's layout. It has an `index.html`, and a
>   `src/main.tsx` that imports `react`, `react-dom/client` and a CSS file.
>   The real templates stay T01b's.
> - A measurement inside the enforcement job's sandbox, on the same launch
>   path the fixture uses. The test payload hands over to the root's bundler
>   with rule 1's argv and environment. A measuring filter is installed in
>   place of the fixture's allow list, in the build with
>   `enforcement-fixture` only. It still kills every call that the public
>   syscall mapping refuses. Every other call is allowed and leaves a kernel
>   record, the way R02ad attributes refusals (0056). The job runs the
>   template, the template with a bad import, and a larger generated input,
>   each several times. It records the union of the calls as an artifact,
>   and fails if any run is killed.
>
> Measuring inside the sandbox sees what the sandbox changes: the
> descriptor limit is already fixed, `/input` has no parent directory with
> its own `node_modules`, and the bundler resolves the packages through
> rule 1's environment alone. No profile is added, so no `react-v1` job
> exists yet. A call that the vDSO serves leaves no record, yet on
> another host or kernel the same code can make the real call. So the
> filter is the measured set plus the vDSO's calls (`clock_gettime`,
> `gettimeofday`, `time`, `getcpu`), each listed as unmeasured with its
> reason, as the fixture's table already does for `clock_gettime`. R03bcbb
> adds `react-v1`, with that set as its filter,
> plus its argv, its environment and the enforcement cases of its row. Rule
> 23's first assumption is split the same way: R03bcba shows that the calls
> fit the boundary, and R03bcbb shows the limits. Rules 4, 22 and 23 and the
> slice table are reworded to match.

> **Note, 2026-11-14 (R03bcba).** `scripts/tools/react-v1.lock` names five
> archives of the npm registry: the bundler's platform archive
> (`@esbuild/linux-x64` 0.28.2, statically linked), the bundler's main
> archive of the same version, only for its licence, which the platform
> archive does not carry, and `react` 19.3.1, `react-dom` 19.3.1 and
> `scheduler` 0.28.0. Each line gives the version, the SHA-256 of the archive,
> its URL and its licence (MIT for all five). `package-root.sh SERVER [LOCK]`
> downloads and checks every archive before it makes the root. It unpacks an
> archive only when it holds nothing but directories and regular files under
> `package/`. It installs the bundler as `bin/esbuild`, each package under
> `modules/<name>`, and each licence under `licenses/`. The provider is
> unchanged for jobs. The test build and the `enforcement-fixture` build know
> one more entry beside the profiles, the bundler measurement. It has no
> store name, so no job can name it, and its payload is the root's bundler
> with rule 1's argv and environment. The bootstrap starts the bundler
> directly; no test payload sits in between. Its filter is `Measure`. The
> first program allows the fixture's allowlist, plus the calls listed in
> the root's `etc/measured-calls`, and logs every other call. The second
> program kills `clone3` and the native calls the probes of the public
> mapping refuse, except the `default` class and calls that the fixture
> allows in some form. It is installed last because it kills `prctl` and
> `seccomp`, which installing a program needs; the first run on the runner
> showed this. The provider reads the same file for the filter's digest, so
> the evidence names the program the bootstrap installs. The job measures
> in rounds. Each round writes the
> calls found so far to that file and runs the template, a bad import and
> 200 generated modules three times each. A round that records nothing new
> and loses no kernel record ends the measurement. Its list is the artifact
> `bundler-calls.txt`: the calls beyond the fixture's allowlist that
> R03bcbb's filter must add. A kill, a call of another architecture or a
> refused form of `clone`, `prlimit64`, `socket` or `setsockopt` fails the
> job. The job also refuses a wrong SHA-256 before anything is written, and
> refuses a changed file of the `react` package as a modified root.

> **Note, 2026-11-14 (R03bcbb).** The test build and the build with
> `enforcement-fixture` know `react-v1`: the root's bundler with the argv and
> environment of the measurement, under the filter `ReactV1`. Its refusal is
> the fixture's. Its allowlist is the fixture's with twelve calls added. The
> first nine are the measured ones of the artifact `bundler-calls.txt` of the
> first passing run: `ioctl`, `nanosleep`, `getpid`, `getcwd`, `epoll_ctl`,
> `tgkill`, `epoll_pwait`, `eventfd2` and `epoll_create1`. The other three are
> the vDSO's `gettimeofday`, `time` and `getcpu`, listed as unmeasured; the
> fixture already allows `clock_gettime`. The measurement records a call but
> not its arguments, and `ioctl` reaches the handler of every descriptor, so
> it is allowed only for the two read-only terminal queries the bundler makes
> before it prints (`TCGETS` and `TIOCGWINSZ`). The template run shows that
> this is enough. Every other added call is allowed whatever its arguments. Keeping the fixture's allowlist under the
> added calls follows from the measurement: it logged only calls outside
> that list, so it says nothing about which of the fixture's calls the
> bundler leaves unused. The esbuild option `--sourcemap` takes no `false`
> value, so the argv leaves it out; esbuild writes no source map unless asked,
> which is rule 1's "no source maps". The bundler also reads resolver
> configuration from the files above each module it imports. A
> `tsconfig.json` or `jsconfig.json` can map an import to a module of the
> revision, a `package.json` can remap it through its `browser` or `imports`
> field, and a `node_modules` directory can shadow `NODE_PATH`. Each of
> them would let a revision import a module that rule 2 refuses. So
> `react-v1` refuses an input that holds a file of those names or any file
> under a `node_modules` directory, at any depth and in any ASCII case. The
> refusal comes before anything is staged, and the attempt then ends as an
> input refusal. The worker reports it as unavailable until R03cb gives builds
> their own failures (rule 18). The enforcement job (part 16) shows:
>
> - the template builds and exits 0 in an attempt group that reads back the
>   limits of 0042 rule 5. Its output is collected within the output limits
>   and refused only for its missing `index.html`, which R03cb's composition
>   adds (rule 8). This proves rule 23's assumption for R03bcbb, so 0042 is
>   not amended;
> - a bad import exits 1 with the one message that names it, and the same
>   import mapped to a module of the revision by `tsconfig.json` is refused
>   before it is staged;
> - a call outside the filter is killed with exactly its record. The bundler
>   cannot be made to make such a call, so an entry with no store name runs
>   the fixture's payload under `react-v1`'s filter, and its thread calls
>   `unshare`;
> - a changed file of the `react` package refuses the `react-v1` attempt as a
>   modified root. Admission checks the whole root before any profile matters,
>   so this shows the root check that `react-v1` runs behind, as part 5 does
>   for the fixture.

> **Note, 2026-11-15 (R03c split).** R03c is split into R03ca, R03cb, R03cc
> and R03cd, because its rules touch four layers that can each be proven on
> their own: the store, the worker, the preview origin and the trusted
> worker's claim loop. The earlier notes, rules 7 and 9 and the R03bb row now
> name the slice that does each part they mention.
>
> - **R03ca, the store.** Rule 9 and the store half of rules 10 to 13 and 18:
>   - schema 15, with the failure values of rule 18 in `jobs.failure`;
>   - one operation that publishes a build with its job's success;
>   - the first build kept, and the eight-build bound. The bound must never
>     remove the latest build of rule 15, so R03ca also adds the store query
>     for that build, which R03cc then uses;
>   - the reference predicates, purge, trash, backup and restore;
>   - the store side of diagnostics: the newest failed job keeps them.
>     No worker calls it yet. Rule 23's assumption about a 25 MiB publication
>     belongs here, since it is a store transaction.
> - **R03cb, the worker.** Rules 7 and 8, and the worker half of rules 11, 13
>   and 18. The trusted worker composes the result, splits the bundler's
>   messages into diagnostics, names each failure and publishes through
>   R03ca's operation. Rule 23's assumption about the bundler's messages
>   belongs here. R03cb also settles the input refusal of the R03bcbb note.
>   A revision that holds resolver configuration ends as `build_failed`,
>   with one fixed diagnostic that names the first such path. It is the
>   user's project that is wrong, not the host, so `execution_unavailable`
>   would mislead. This adds a third case to rule 18's `build_failed`.
> - **R03cc, the preview.** Rules 14 to 16 below the wire, and the read rule
>   of 13:
>   - the build target of the management preview operation;
>   - build tickets in the ticket registry, with their revocation;
>   - the `/b/{ticket}/` route;
>   - the head status, built on R03ca's latest-build query, whose
>     diagnostics only a caller who may read the job's revision is given.
>     R03d puts the ticket and the status on the API, MCP and the CLI.
> - **R03cd, the deadline.** Rule 20: the trusted worker ends a `react-v1` job
>   that no execution worker claimed within ten minutes, and
>   `GET /v1/background` reports whether an execution worker is ready. It
>   ends the job as `execution_unavailable`, which schema 14 already has, so
>   it depends on no other R03c slice.
>
> The order is R03ca, R03cb, R03cc; R03cd can land at any point after R03bcbb.
> Rule 22 already covers R03cd, which changes the trusted worker's loop, so
> R03cd passes the enforcement job too. Rule 23 and the slice table are
> reworded to match.

> **Note, 2026-11-15 (R03ca).** The store half is implemented, and no worker
> calls it yet. It is a port of its own, `BuildStore`, beside `JobStore`, so
> the worker's port and its fakes are unchanged until R03cb.
>
> - Schema 15 is rule 9 as written. `job_diagnostics` holds a path, a line
>   and a column together or none of them: a fixed diagnostic (a filter
>   kill, resolver configuration) points at no file.
> - `publish_build` makes the checks of `complete_profile_job` (0057 rule
>   12), with the output's file and byte counts as the job's result, in the
>   transaction that stores the build. A refusal stores no file. An existing
>   build for the artifact, revision and profile is kept and returned
>   (rule 10).
> - The bound of eight removes builds oldest first by publication time,
>   then by revision. It skips the latest build of each profile and the one
>   being published.
> - Only `fail_build` records rule 18's three failures; `fail_job` refuses
>   them. `build_failed` takes up to 20 diagnostics and the other two take
>   none. `fail_build` makes the attempt checks of publication (0057 rules 7
>   and 12) without a binding, so a revoked attempt, a requested cancel
>   included, stores no diagnostics.
> - "The newest failed job" is the newest by enqueue order, the order of
>   rule 16. When a `run_profile` job fails, by any path, the diagnostics of
>   the artifact's jobs of that profile enqueued before it are deleted, in
>   the transition every failure shares. A job enqueued before one that has
>   failed already keeps none. A failure of an older revision's job enqueued
>   later still deletes the head job's diagnostics, as rule 13 says; R03cc's
>   head status then reports `failed` without diagnostics.
> - The store's build and diagnostic reads return nothing for an artifact
>   in the trash or purged, and give the rows back after a recovery. That
>   is the store side of "Trash keeps them and serves none". Who may read a
>   job's diagnostics is R03cc's read rule.
> - Rule 23's assumption holds on the development host. A publication of
>   25 MiB (six files, the package limit) took about 0.6 s in a debug build
>   and 0.14 s in a release build, under a load average below 2, against
>   the worker's default lease of 30 s. The test asserts half the lease.

> **Note, 2026-11-15 (R03cb).** The worker half is implemented. The worker
> is given the build profiles of its build with the profiles it knows. A job
> of a build profile ends through R03ca's `BuildStore`; a job of any other
> profile, the fixture's, keeps the ends of 0057, so each of its failed runs
> is still `internal`.
>
> - The composition of rules 7 and 8 is a pure function of the core. The
>   bundle must be `app.js` and at most `app.css`. The revision's
>   `index.html` must be UTF-8 and hold the element of rule 7 exactly once, as
>   markup a browser runs: not inside another tag, a comment, or a raw text,
>   inert or foreign element that is still open there (such as `<template>`,
>   `<noscript>`, `<textarea>` or `<svg>`); a document that leaves this open
>   is refused.
>   "Refers to a path under `src/`" is read as a `src/`, in any ASCII case,
>   that starts a path: at the start of the text or after a quote, `=`,
>   space or parenthesis, past any `./`, `../` or `/`. `assets/src/` and a
>   URL's `/src/` are other directories. "Outside `src/`" folds case too, so
>   no case of the directory is published. Only `index.html` is checked, as
>   rule 7 says. A path the bundle and
>   the revision both hold, or two that differ only by case, ends the job as
>   `output_rejected`, as does a result that breaks another package rule.
> - A build profile's output is collected under the package rules without
>   the entry document, which the composition adds. The domain gains
>   `FileSet` for such a set, and the resolver configuration rule of the
>   R03bcbb note moves into the domain, so that the provider's refusal and
>   the worker's diagnostic name the same files.
> - The split of rule 13 reads each error header, `✘ [ERROR] <text>` (or
>   `X [ERROR]`), and takes the first non-empty line after it as the
>   location when it reads `<path>:<line>:<column>:` and the path lies under
>   the input mount: `/input/`, or `input/` after one or more `../`, since
>   the bundler prints paths relative to its working directory, `/tmp`. A
>   bare `input/` lies under `/tmp` and is no location. Any other location
>   is dropped and the message kept, as are warnings, notes, the quoted
>   excerpt and the closing count. A mount that starts a path in a message
>   is rewritten as rule 13 says, and a specifier such as
>   `./components/input/field` is kept as written; a control
>   character becomes a space and the text is cut at a character boundary
>   within 512 bytes. A non-zero exit with no message that can be read keeps
>   one fixed diagnostic instead, so a failed build always says something.
> - The ends: a non-zero exit is `build_failed` with the split messages; a
>   filter kill is `build_failed` with its fixed diagnostic; resolver
>   configuration is `build_failed` with one fixed diagnostic that names the
>   first such path in path order, which settles the input refusal of the
>   R03bcbb note (the provider reports it as a launch failure of its own); a
>   limit is `limit_exceeded`; an output that collection or composition
>   refuses is `output_rejected`. A refusal before the payload stays
>   `execution_unavailable`, another failed verdict or an output bound to
>   another run stays `internal`, and a stop stays a lost lease.
> - The `react-v1` filter check of the R03bcbb note now has the store name
>   `react-v1-check` and is a build profile, in the test build and the
>   `enforcement-fixture` build only, so that the enforcement job can show a
>   filter kill and a limit through the worker; the bundler cannot be made
>   to do either. A release build knows neither profile (rule 22).
> - The test template's `index.html` gains the element of rule 7.
> - The dispatch tests run on any host with a scripted port. Part 17 of the
>   enforcement job runs each case through the real worker: the template is
>   published as `app.css`, `app.js` and `index.html`; a bad import is
>   `build_failed` with one diagnostic, `src/main.tsx`, line 1, column 16,
>   `Could not resolve "left-pad"`, which is rule 23's assumption for R03cb;
>   resolver configuration and a missing script element end as above; and
>   the `react-v1` check ends as `build_failed` at a filter kill and as
>   `limit_exceeded` at the log bound.

> **Note, 2026-11-15 (R03cc).** The preview half is implemented below the
> wire. No API route, MCP tool or CLI command issues a build ticket or reads
> the head status yet; R03d adds them.
>
> - The management preview operation takes a target: the revision's source,
>   under `/p/`, or its build of a profile, under `/b/`. A revision without
>   that build is refused at issue (`not found`), before a ticket exists. A
>   build ticket lives in the same registry as a source ticket, under the
>   same per-actor bound. Token revocation and actor revocation remove it as
>   they remove a source ticket. Each file read checks access again, so the
>   trash stops it, and a recovery serves it again, as it does a source
>   ticket.
> - `/b/{ticket}/` serves the build's `index.html`, and `/b/{ticket}/<path>`
>   serves its other files, with the headers of `/p/`: the CSP sandbox, the
>   media type of the path, `nosniff` and `no-store`. A build ticket serves
>   nothing under `/p/` or `/r/`, and a source ticket serves nothing under
>   `/b/`. One file is read per request, not the whole build.
> - The head status is read from one store snapshot, so a revision
>   committed meanwhile cannot make the latest build newer than the head. It
>   holds the head revision, the latest build (rule 15), the newest `run_profile` job of the profile for the head
>   revision by enqueue order, and that job's diagnostics. When the latest
>   build is the head's, the status is built. Otherwise it is:
>   - `pending` for a queued, retry-scheduled or running job;
>   - `failed`, with the job's failure and diagnostics, for a failed job;
>   - none when there is no job, or the job was cancelled or succeeded
>     without a build.
>     The diagnostics are empty when a later failure of the profile deleted
>     them, as the R03ca note says.
> - Only the newest job for the head sets the status, as rule 16 says: an
>   older job for the head that is still running does not, even when the
>   newest was cancelled.
> - The read rule of 13: the status, and with it the diagnostics, is given
>   only to a caller who may read the artifact and so its head revision, the
>   job's. Every other caller, and every caller while the artifact is in the
>   trash, gets `not found` for the status, a build preview and a build file.
>   Diagnostics are read only through the status. R03d attaches them to the
>   job reads under the same rule.
> - The enforcement job's template case of part 17 issues a build preview
>   of the published build through the operation. It checks that `/b/`
>   serves each of the build's files, and its entry document, with the
>   build's bytes, the media type of the path and the CSP sandbox. Only the
>   `enforcement-fixture` build has a library call that issues one without a
>   route.

> **Note, 2026-11-15 (R03cd split).** R03cd is split into R03cda and
> R03cdb. The readiness of rule 20 needs a channel that no store has yet:
> the execution worker runs in its own unit under another account (0057
> rule 18) and shares only the database with `serve`. The deadline needs
> the readiness. Counted from enqueue alone, it would end the backlog of a
> healthy execution worker that is busy with other builds, or a job
> recovered from the trash while a worker is ready. So this note amends
> rule 20: the deadline applies only while no execution worker is ready,
> any worker's claim loop applies it, and it leaves a job in the trash
> alone. R03cda comes first.
>
> All workers and `serve` run on one host and share one local `SQLite`
> file, so their times come from one clock. A step of that clock is not
> corrected; it can move the five-minute window by the size of the step.
>
> - **R03cda, the readiness.** Schema 16 adds one table, `execution_workers`,
>   with a worker identity and the time it was last seen. It holds no user
>   data and no path.
>   - An execution worker writes its row each time it is given capacity
>     (0057 rule 19), before it claims, and each time it renews a running
>     job's lease, in the renewal's transaction. Only a row last seen a
>     minute or more before is rewritten, so an idle poll writes at most
>     once a minute. A holder that runs a long attempt, cleanup included,
>     keeps its row fresh through its renewals.
>   - It deletes its row when admission or reconciliation refuses it.
>   - A busy lock writes nothing, since its holder keeps its own row.
>   - Each write also deletes the rows of every worker last seen more than
>     an hour before.
>   - An execution worker is ready when its row was last seen within the
>     five minutes before now, or later than now. `GET /v1/background`
>     gains `execution: { ready }`, which is `true` when any worker is.
>   - Backup copies the table with the database, and restore empties it in
>     the restore's transaction, since no worker of the backed-up database
>     serves the restored one.
>   - The contract gains the field, as contract 0.29.0; R03d bumps from
>     there. Proven by store and
>     core tests with the injected clock (the row at capacity and at a
>     renewal, the one-minute rewrite, the refusal, the pruning, the
>     window's edges and a restore that empties the table), contract and API tests, and the enforcement job,
>     where a worker with capacity makes `ready` true.
> - **R03cdb, the deadline.** In every worker's claim loop, the trusted
>   worker's included, before the claim, one store transaction ends as
>   `execution_unavailable` each queued `run_profile` job that meets six
>   conditions:
>   - its profile is a build profile that the worker's build knows;
>   - it was enqueued ten minutes or more before now;
>   - no execution worker is ready, as R03cda reads it;
>   - it has no attempt row;
>   - its artifact is not in the trash;
>   - its actor is enabled, as the claim requires (0036), so a disabled
>     user's job stays queued and resumes once the user is enabled again.
>     A read decides first whether any job qualifies, so an idle poll takes
>     no write lock. The ending runs in a write transaction that checks the
>     conditions again, and the claim takes the same lock, so a job is
>     either claimed or ended, never both. A job that has an attempt row,
>     even one that a graceful release gave back, is left to 0057. Each
>     end goes through the store's job-change recording, which keeps
>     `run_profile` out of the change feed until R03d makes the kind public,
>     as it does for every other change of such a job. A
>     build that knows no build profile ends nothing, and so does a release
>     build until R03f. Proven on any host with the injected clock: the
>     deadline's edge, a ready worker that keeps an old job queued until its
>     row is five minutes old, a job with an attempt row, a job in the trash,
>     a disabled user's job, a profile that is not a build profile, an execution worker's loop that
>     ends a job as the trusted worker's does, and a job claimed before the
>     deadline that is not ended. The enforcement job runs unchanged.
>
> Rule 22's enforcement requirement covers both, since both change a
> worker's loop.

> **Note, 2026-11-15 (R03cda).** The readiness is implemented as the split
> note describes it.
>
> - Schema 16 adds `execution_workers` (a worker identity and a time). The
>   upgrade from 15 only creates the table.
> - The port is `ExecutionPresence`, beside `JobStore`, with three calls:
>   - `mark_seen` reads first, so an idle poll with a fresh row takes no
>     write lock;
>   - `forget_worker`;
>   - `execution_ready`.
>     Only `run_next_with`, the worker that can hold an execution port, is
>     given it.
> - The worker calls them around capacity, and only when its build knows a
>   build profile, since only such a worker can run a build:
>   - it marks itself seen once capacity is held, at the time read then,
>     before reconciliation and the claim;
>   - at a refusal it reports the refusal, then forgets its row (a read
>     first, so a refused worker without a row takes no write lock);
>   - it writes nothing when the lock is busy or it has no port.
> - Every checkpoint of a running `run_profile` job's attempt, a stopping
>   one included, marks the worker the job row names, in the checkpoint's
>   transaction, under the same one-minute rule. A long cleanup therefore
>   keeps its worker ready.
> - A row up to five minutes later than now counts as ready, and the
>   pruning deletes rows more than an hour before or after now, so a clock
>   that stepped back is bounded. A worker that restarts under a new
>   identity leaves its old row, which stops counting within five minutes.
> - The restore that expires leases also empties the table in the same
>   transaction.
> - `GET /v1/background` reads the readiness at the server's clock and adds
>   `execution: { ready }` (contract 0.29.0). The field is required, and the
>   TypeScript client refuses a body without a boolean `ready`. A store that
>   cannot be read reports `ready: false`, since no build can start then,
>   and keeps the server's worker status. The web and the CLI do not show
>   the readiness yet.

> **Note, 2026-11-15 (R03cdb).** The deadline is implemented as the split
> note describes it.
>
> - `BuildStore::expire_unclaimed` ends, in one write transaction, each
>   queued `run_profile` job of the given build profiles that:
>   - was enqueued ten minutes or more before now (`CLAIM_DEADLINE_MS`);
>   - has no attempt row;
>   - has its artifact in place;
>   - belongs to an enabled actor;
>     and only while no `execution_workers` row is within the readiness
>     window of now.
> - A read decides first whether any job qualifies, so an idle poll takes
>   no write lock.
> - Each end is recorded through the job-change recording, which keeps
>   `run_profile` out of the change feed until R03d. The worker reports
>   each end as a settled claim event.
> - Every worker's loop calls it after asking for capacity and before the
>   claim. An execution worker given capacity has therefore marked itself
>   seen first and ends nothing; it claims the old job instead.
> - A worker without the build store, or a build that knows no build
>   profile (a release build until R03f), ends nothing.
> - A running job always has an attempt row, so the queued-state
>   condition and the attempt condition guard it twice.
> - The readiness condition is the one the readiness read uses, a row
>   within the window before or after now, and the profiles are compared in
>   the same SQL, so the read that skips the write and the write agree.
> - A worker process applies the deadline only from one readiness window
>   after its start (`WorkerConfig::deadline_from`). At a start, or after a
>   restore empties `execution_workers`, an execution worker that runs has
>   not marked itself seen yet; without this wait the first poll of the
>   trusted worker would end the whole backlog. The wait only delays an end,
>   so it never ends a job that rule 20 keeps.
> - Known limitation: an execution worker marks itself seen when it is given
>   capacity and when it renews a build's lease, not while it runs a verify
>   job. A verify job reads one bounded revision, so it ends well within the
>   readiness window; a verify renewal cannot mark the row, because the
>   trusted worker runs verify jobs too and must never look ready.

> **Note, 2026-11-15 (R03d split).** R03d is split into R03da, the public
> kind, and R03db, build tickets and the head status. Each covers the
> contract, the API, MCP, the TypeScript client and the CLI for its part, so
> that HTTP, MCP and the CLI keep one behavior; the web stays R03e. R03da
> comes first, since a build ticket and the status name a build that only a
> public job can request. This note fixes the wire shapes that rules 14, 16
> and 17 leave open; it changes no numbered rule. In the earlier notes,
> R03d now means R03da for the public kind, the change feed and the
> diagnostics on the job reads, and R03db for build tickets and the status.
>
> - **R03da, the public kind (contract 0.30.0).**
>   - `JobKind` gains `run_profile`. `EnqueueJobRequest` gains `profile`,
>     which `run_profile` requires and `verify_revision` refuses
>     (`invalid_request`). The idempotency fingerprint already includes it.
>   - Only a build profile of the server's build is public (rule 17:
>     `react-v1`). Any other profile, including one the build knows only
>     for its tests (`fixture`, `react-v1-check`), answers
>     `profile_unknown` (`422`), and nothing is enqueued or recorded. A
>     release build knows none until R03f.
>   - `Job` gains `profile`, present exactly for `run_profile`, and
>     `diagnostics`, present only on a failed `run_profile` job that has
>     some: at most 20, each a `message` and an optional `location` of
>     `path`, `line` and `column`. A job read already answers only the
>     actor who enqueued the job while it may read the artifact outside
>     the trash (0030), which is the read rule of 13, so the diagnostics
>     follow the job read and need no check of their own.
>   - The wire `JobFailure` gains `execution_unavailable`, `build_failed`,
>     `limit_exceeded` and `output_rejected`.
>   - A succeeded `run_profile` job gains `build`, with the file and byte
>     counts that the store keeps as its result (the R03ca note), in place
>     of `result`, which stays the verification's. Its revision's build is
>     previewed with R03db's route.
>   - The job reads, cancel and the change feed cover a `run_profile` job
>     of a build profile; one of another profile stays
>     hidden as 0057 rule 3 hides it (`404`, out of the lists and the feed).
>     Cancel is that of 0030 rule 6: a running build stops at its next
>     checkpoint and is `cancelled` after its cleanup (0057). The feed
>     records changes from R03da on; earlier ones are not backfilled.
>   - The actor's 16 open jobs count exactly the jobs the wire shows: every
>     `verify_revision` job and every `run_profile` job of a build profile.
>     A hidden job is not counted, as 0057 rule 3 says, since its actor can
>     neither see nor cancel it.
>   - MCP: `enqueue_job` takes `profile`, and the job tools and the server
>     instructions describe the kind. The client parses the kind, the
>     fields and the failures, and its enqueue takes the profile. The CLI
>     gains `artifree job enqueue <artifact-id> --revision <n> --build
<profile>`; `--profile` keeps naming the connection profile. The web
>     only names the new kind and failures, with no build action; that
>     change passes the component tests and the Chromium and Firefox smoke.
>   - Proven by contract tests, and by API, MCP, client and CLI tests: an
>     enqueue and its replay, a key reused with another profile, an unknown
>     profile and a release build's refusal, a profile on `verify_revision`,
>     a profile the build knows only for tests, the open-job bound across both
>     kinds with a hidden job left out, the reads, the feed and cancel, the
>     build counts, and diagnostics on a failed job; and web component tests
>     for the new names.
> - **R03db, build tickets and the head status (contract 0.31.0).**
>   - `POST /v1/artifacts/{id}/revisions/{revision}/builds/{profile}/previews`
>     issues a build ticket through the management preview operation with
>     the build target and answers a `Preview` (`201`). A revision without
>     that build is `not_found`, and a profile that is not a build profile is
>     `profile_unknown`.
>   - `GET /v1/artifacts/{id}/builds/{profile}` reads the head status of
>     rule 16: the profile, the head revision, the latest build's revision
>     if any, `status` (`built`, `pending`, `failed` or `none`), and for
>     `pending` and `failed` the job's id, with the failure and the
>     diagnostics for `failed`. A profile that is not a build profile is
>     `profile_unknown`. The read rule of 13 applies as R03cc implements
>     it: any other caller, and every caller while the artifact is in the
>     trash, gets `not_found`.
>   - MCP: `create_preview` takes an optional `profile` for a build preview,
>     and a read tool `get_build_status` gives the status. The client gains
>     both calls. The CLI's `artifree artifact preview` gains `--build
<profile>`, and `artifree artifact build-status <id> --build
<profile>` prints the status.
>   - Proven by contract, HTTP, MCP, client and CLI tests for both calls and
>     their refusals; the `/b/` route itself is proven by R03cc.

> **Note, 2026-11-15 (R03da).** The public kind is implemented as the R03d
> split note describes it, with these choices.
>
> - Which jobs are public is one fixed condition, the same in every build:
>   a verification, or a `run_profile` job of `react-v1`
>   (`artifree_domain::job::PUBLIC_PROFILE`). The store applies it to the
>   job read, the list, cancel, the `job_changed` row and the open-job
>   count, so a job of a profile known only for tests stays hidden there.
>   A release build that holds a `react-v1` job from a restored database
>   shows it; its worker ends it as `execution_unavailable`, as 0057 rule 1
>   says.
> - Only the enqueue depends on the build: `ArtifactService::enqueue_build`
>   accepts `react-v1` only when the server's build profiles hold it
>   (`Settings::build_profiles`, the build's own list by default), before
>   anything is read or written. A name that cannot be a profile answers
>   `profile_unknown` too and is not quoted back.
> - The bound limits only a public enqueue. A hidden job, which only tests
>   enqueue through the core, is neither counted nor bounded.
> - `Job.diagnostics` is read with the job, for a failed `run_profile` job,
>   in the same request as the job read. `Job.build` holds the counts the
>   store keeps as the result of a succeeded `run_profile` job.
> - The client's job parse is closed: a build without its profile, a
>   verification with one, a verification result on a build, or a build's
>   counts or diagnostics on a verification is an invalid response. The
>   web only names the kind (`Build`) and its four failures; the build
>   action, a build's counts and its diagnostics in the web stay R03e.
> - Known limitations. A retry of a build after the server's build stopped
>   knowing the profile (a restart as a release build) answers
>   `profile_unknown` instead of its replay, since the profile is checked
>   before the key; the job is still read and listed. A list reads the
>   diagnostics of each failed build on its page with its own read, after
>   the page, so a build that fails meanwhile can empty an older job's
>   diagnostics in that answer, as rule 13 empties them for every later
>   read.

> **Note, 2026-11-15 (R03db).** Build tickets and the head status are on
> the wire as the R03d split note describes them, with these choices.
>
> - One check decides which profiles a request may name, for the enqueue
>   of a build, a build preview and the head status alike
>   (`artifree_core::jobs::require_build_profile`): `react-v1`, where the
>   server's build profiles hold it. The core's `enqueue_build`,
>   `preview_build` and `build_status` each run it, before anything is
>   read, so every caller shares it and a
>   profile that is not a build profile answers `profile_unknown` whatever
>   the artifact is, and the name is not quoted back. An artifact that
>   cannot be read is then `not_found`, as is a revision without that
>   build, at issue.
> - `BuildStatus` is one closed object: `profile`, `head_revision`,
>   `latest_build_revision` when there is a latest build, `status`, and
>   `job_id`, `failure` and `diagnostics` only where the split note puts
>   them. The client refuses any other combination, diagnostics on a
>   failure other than `build_failed` or more than 20 of them, and a latest
>   build newer than the head, as an invalid response.
> - MCP's `create_preview` takes `profile`; without it the tool previews
>   the revision's files as before. `get_build_status` is a read tool.
> - The CLI's `artifact preview --build <profile>` previews the current
>   revision's build unless `--revision` names another, as the source
>   preview does; a current revision without a build is `not_found`, and
>   `artifact build-status` names the latest build to preview instead.
> - The isolation tables cover both routes and the tool: another user
>   gets the answer for an artifact nobody holds. The holder's build
>   preview there is `not_found`, since no worker publishes a build in
>   that test. With a published build, `build_status_api.rs` proves that
>   another user gets the answer for an artifact nobody holds while the
>   owner gets the ticket, and MCP proves the owner's ticket too.
> - The web request gate still knows only the source preview route; the
>   web's build action, status and diagnostics are R03e.

> **Note, 2026-11-15 (R03e split).** R03e is split into R03ea, the build
> in the Jobs tab, and R03eb, the build preview. Each changes the web only,
> on the calls R03da and R03db put on the wire, and each passes the
> component tests and the Chromium and Firefox smoke that rule 22 asks of
> R03e. R03ea comes first, since R03eb's notice uses the head status that
> R03ea puts in the web; a build itself can also be asked for through the
> CLI and MCP. This note fixes what rule 14, rule 16 and the table leave
> open for the web; it changes no numbered rule, and in rule 22 and the
> earlier notes R03e now means both slices.
>
> - **R03ea, the build in the Jobs tab.**
>   - The tab gains "Build revision N" beside "Verify revision N", for the
>     revision the page shows. It asks for a `run_profile` job of `react-v1`
>     with the press rules of 0055 rule 14: a new request key for each
>     press, the same key for a retry of a lost answer, and its outcome
>     shown in place.
>   - Above the list, a banner shows the head status of rule 16 from
>     `GET /v1/artifacts/{id}/builds/react-v1`. It names the head revision
>     ("Revision H, the newest") and says built, pending, failed with the
>     failure in words and the job that failed, or none, and it names the
>     latest build's revision when that is not the head.
>   - The status is read when the tab opens, again on every `job_changed`
>     and every `revised` change of the artifact, and with the list reads of
>     0055 rule 15. A `profile_unknown` answer is not read again until the
>     change feed resets.
>   - When a build of the shown revision stays queued past 5 seconds, the
>     tab reads `/v1/background`; when `execution.ready` is false it says
>     that no execution worker is ready, so the build waits and ends as
>     `execution_unavailable` after ten minutes.
>   - A build row of the list shows its file and byte counts once it
>     succeeded, and its diagnostics once it failed with some. Diagnostics
>     are shown there only; the banner shows the failure in words and names
>     the job.
>   - Diagnostics are shown as plain text, each with its path, line and
>     column when it has a location; they come from the bundler and can
>     quote the artifact's source, so they are never rendered as markup or
>     links.
>   - `422 profile_unknown`, the answer of every release build until R03f,
>     is shown as such: the status says that this server cannot build the
>     profile, and a press of "Build revision N" shows the refusal with its
>     code.
>   - Proven by component tests of the status, the build rows and the
>     diagnostics. The smoke runs against a release build, where the status
>     and the press answer `profile_unknown`; since no smoke host runs a
>     profile, it gives the tab pending and failed builds, with their rows
>     and diagnostics, by intercepting the status, the enqueue, the job
>     list and the job reads in the browser.
> - **R03eb, the build preview.**
>   - The page's preview gains a choice between the revision's files and
>     its build.
>   - When the shown revision is the head, the choice follows the head
>     status: a built head is previewed; otherwise the latest build, older
>     than the head, is previewed with a notice that names that build's
>     revision and the head's status (pending, failed or none), so a failed
>     or newer save never hides the last good build (rule 15). With no
>     build at all, it says so and issues no ticket.
>   - When the shown revision is not the head, the choice asks for a ticket
>     for that revision's build. On `not_found` it says that this revision
>     has no build; it never falls back to a newer build.
>   - The web's request gate learns the build preview route as a read, as
>     it knows the source preview route.
>   - `profile_unknown` is shown in place of the build choice's preview.
>   - Proven by component tests of the choice and the notice. The smoke
>     proves the choice and the refusal on a release build, and the notice
>     with an intercepted status; a successful build and its `/b/` preview
>     stay proven by component tests and by the enforcement job at the
>     HTTP level, as the table says.

> **Note, 2026-11-15 (R03ea).** The Jobs tab implements R03ea as the R03e
> split note fixes it; no numbered rule changes and the wire is unchanged.
>
> - "Build revision N" asks for `run_profile` with `react-v1` under the press
>   rules of 0055 rule 14, in its own slot beside "Verify revision N". A
>   committed build reads the head status again.
> - The banner reads `GET /v1/artifacts/{id}/builds/react-v1` with the tab's
>   other reads, one at a time, after the jobs and before
>   `GET /v1/background`. A change that a status read began before makes its
>   answer stale: it is read again, not shown. A `job_changed` of a job the
>   list shows as a verification does not read the status, since it cannot
>   change a build and would otherwise keep every status read stale while a
>   verification runs. A reset of the feed drops the status with the rows,
>   and only then is a `profile_unknown` answer read again.
> - The banner is a polite status region, since it stays on screen and
>   changes with the feed; a failed read of it is an alert.
> - The notice of a queued build follows `execution.ready` alone, and only a
>   build of the shown revision shows it; the notice of a queued verification
>   still follows the server's worker.
> - Diagnostics are React text nodes in the build row, each with its
>   `path:line:column`; the banner never shows them.
> - Proven by the driver, model and row tests, each guard by a mutation that
>   fails them, and by the Jobs smoke cases `build-unknown` (the release
>   build's `profile_unknown`, on the status and on a press) and
>   `build-intercepted` (pending, the execution notice from the server's real
>   `GET /v1/background`, and failed with diagnostics as text) in Chromium and
>   Firefox. R03eb, the build preview, is not implemented.

> **Note, 2026-11-15 (R03eb).** The preview implements R03eb as the R03e
> split note fixes it; no numbered rule changes and the wire is unchanged.
>
> - The preview gains "Show: Files | Build (react-v1)", kept while the page
>   shows the artifact. The build is shown in a plain frame with the
>   preview's sandbox on the content origin; it has no runtime view, so no
>   capability or bridge reaches it.
> - A revision older than the newest one the page knows gets a ticket for its
>   own build, without a status read; `not_found` says "Revision N has no
>   build." The newest revision reads the head status and follows the note:
>   a built head is shown, the latest older build is shown with a notice that
>   names it and the head's status, and with no build at all no ticket is
>   asked for. A status whose head is newer than the shown revision makes it
>   an older one; a status of an older head waits for the next read.
> - The status is read again on each `job_changed` and `revised` change of
>   the artifact and after a reset, and not after `profile_unknown` until a
>   reset. A ticket is asked for again only when the build to show changes
>   or "Refresh preview" is pressed.
> - The request gate holds `POST .../builds/{profile}/previews` as a read,
>   as it holds the source preview.
> - Proven by the model tests of the choice and the words, the request gate
>   tests, and the Jobs smoke cases in Chromium and Firefox:
>   - `preview-build-unknown` runs on the release build: `profile_unknown`,
>     no ticket and one status read.
>   - `preview-build-notice` intercepts the status, the tickets and the build
>     page. The notice names revision 1 while revision 2 is pending, and only
>     revision 1's ticket is asked for. A real `job_changed` then reads a
>     built head, so the notice goes and revision 2's build is asked for.
>   - `preview-build-missing` intercepts too: revision 1 without a build is
>     said to have none, and no other ticket is asked for.
>   - A successful build served by `/b/` stays proven by the enforcement job
>     at the HTTP level.

> Builds on [0042](0042-linux-untrusted-execution.md) (the boundary),
> [0056](0056-linux-provider-delivery.md) (the provider),
> [0057](0057-connecting-execution-to-jobs.md) (the `run_profile` kind and
> its worker), [0030](0030-persistent-jobs-and-worker.md) (jobs),
> [0003](0003-local-html-artifact-slice.md) (packages and the preview origin) and
> [0029](0029-retention-and-cleanup.md) (content references and purge).

> **Note, 2026-11-14 (R03a).** Rule 6 is implemented as a 21st media type,
> `Source`, for `.ts`, `.tsx` and `.jsx` in any case. Its `Content-Type` is
> `text/plain; charset=utf-8`, sent with `nosniff`, so the preview origin
> never serves source as script; the type keeps source apart from `.txt`
> for the later slices. `.cjs` stays refused. The runtime SDK's list, the
> shared path fixtures and the published `package_rules` follow, and the
> contract is 0.28.0. Two consequences of sharing the path rules: the
> provider's output collection now also accepts these extensions, which R03cb's
> composition (rule 8) narrows to `app.js` and `app.css`; and, as with the path
> expansion of 0046, the schema stays at 14, so a previous release started on
> a directory that holds such a file reports that artifact as corrupt. Going
> back needs a backup taken before the upgrade (0027).

## Context

R02 ends with an untrusted worker that runs one profile, the repository's
fixture, and only in a build with the `enforcement-fixture` feature
(0057 rules 2 and 21). A release build knows no profile, so every
`run_profile` job there ends as `execution_unavailable`, and no host is
called supported. The collected output is counted and dropped (0057
rule 12). The roadmap row R03 asks for the first real use of that worker:
build React/TSX source with one fixed template and dependency policy, serve
only the output of the revision that was built, keep the last good build
across errors, late results and consecutive saves, and show status and
errors to agents, the CLI and the web.

The earlier records leave these parts to R03:

- **0042 rule 2:** only a reviewed, fixed profile chooses the executable,
  its arguments, its environment, its dependency set and its output root.
  No caller supplies any of them.
- **0042 rule 3:** an operator-installed, versioned tool root with a complete
  digest manifest and licences; no download and no lifecycle script at run
  time; "R03 starts with preinstalled fixed dependencies"; no discovery
  through `PATH`, npm settings, project configuration or a shell.
- **0042 rule 5:** the limits (1 GiB, one CPU, 120 s, 64 tasks, a 128 MiB
  scratch, 256 output files of at most 5 MiB and 25 MiB in total, 1 MiB of
  log, 256 descriptors). Changing them amends 0042.
- **0042 rule 7:** R03 owns durable build bytes and preview selection,
  transactional publication of references, orphan cleanup and the last
  successful preview. A late or older result never replaces a newer
  revision's preview. Logs are untrusted text. No object storage and no
  second source write path.
- **0042 rule 1 and 0057 rule 3:** the public kind and its wire error
  vocabulary.
- **0056 rules 4 and 6:** the server binary refuses until R03 adds the first
  real profile; packaging the tool root for operators is R03's, and so is the
  documentation of the host and the second worker unit.
- **0042 section 8:** R03's presentation changes need Chromium and Firefox
  smoke checks.

Before R03a, packages followed 0003: at most 256 files of 5 MiB and 25 MiB in
total, an `index.html`, and a closed list of 20 media types that has no
`.ts`, `.tsx`, `.jsx` or `.cjs`. The same rules are mirrored in the runtime
SDK and the shared path fixtures, and published as `package_rules`. A
preview ticket is bound to one revision and serves that revision's files
from the separate origin under `/p/{ticket}/` and `/r/{ticket}/`.

## Decisions

Each slice that changes a rule of an earlier record adds a dated note to
that record in the same change, as R02 did; until then the earlier rule
stands.

### The profile

1. **One profile, `react-v1`, whose payload is one bundler.** The payload is
   a statically linked esbuild executable from the tool root. No Node.js
   runtime, package manager or shell is in the root. The bootstrap still
   takes only the profile's identity (0056 rule 3); the profile, not the job,
   holds the rest:
   - argv: bundle `/input/src/main.tsx` into `/tmp/out` as `app.js` and,
     when the source imports CSS, `app.css`; ES module format, browser
     platform, the automatic JSX runtime, minified, no source maps, no
     colour, a bounded message count, and `metafile` off;
   - environment: only the module directory of the root's preinstalled
     packages and a setting that keeps the bundler's runtime within one
     CPU and 64 tasks. No other variable is passed;
   - output root: `/tmp/out`, as for the fixture.
2. **A fixed dependency set.** The root holds `react`, `react-dom` and
   `scheduler` at one pinned version each, unpacked, with their licences.
   An import of any other bare module fails the build. The set changes only
   with a new profile name (`react-v2`) and a new tool root; a job pins its
   profile, so a job enqueued under one set never runs under another.
3. **Its own system call filter.** `react-v1` gets an allow list measured
   from the bundler on the supported host, kept separate from the fixture's
   table. A call outside it ends the attempt (rule 18 names the failure).
4. **The limits of 0042 rule 5 are unchanged.** If the bundler cannot build
   the template within them, R03bcbb amends 0042; it does not raise a limit in
   code.
5. **Which builds know `react-v1`.** This amends 0056 rule 4 and 0057
   rule 2. From R03f, the server built for x86_64 Linux knows `react-v1` in
   its release build; until then only the build with `enforcement-fixture`
   knows it (rule 22). The fixture stays behind `enforcement-fixture`. A server built for
   any other target knows no profile, as today, and refuses an enqueue of
   `react-v1` with `profile_unknown` (rule 17), so a job is never queued
   where no worker could run it. On a known target, a host that fails
   readiness is handled by rule 20.

### The source and the output

6. **Package rules accept source files.** `.ts`, `.tsx` and `.jsx` join the
   media types (served as `text/plain` on the preview origin, never as
   script), together with the runtime SDK's mirror, the shared fixtures and
   `package_rules`. This bumps the contract. The other limits of 0003 do not
   change. A package still needs `index.html`.
7. **A fixed template layout.** A buildable revision has `index.html` and
   `src/main.tsx`; other files under `src/` are bundler input. The
   `index.html` loads the bundle with exactly
   `<script type="module" src="./app.js"></script>` and refers to no path
   under `src/`; R03cb's composition checks both and refuses the build
   otherwise, so a successful build never references a missing file. The
   worker stages the whole pinned revision as today (0057 rule 8). Nothing
   in the revision chooses an entry point, a flag or a dependency.
8. **The trusted worker composes the result.** After a passing verdict
   (0056 rule 5) the worker takes the collected bundler output and adds the
   pinned revision's files outside `src/` by digest. A path that both
   contain, a bundler output other than `app.js` and `app.css`, an
   `index.html` that breaks rule 7, or a result that breaks a package rule
   ends the job as `output_rejected`. The bundler never writes `index.html`.

### Durable builds

9. **Schema 15.** It adds two tables and rebuilds `jobs` once, so that R03ca
   needs no second rebuild:
   - `builds`: the artifact, the revision, the profile, the job, the tool
     digest, the file count, the total size and the time of publication;
   - `build_files`: each path with its size and SHA-256, whose bytes are
     rows of the existing `blobs` table;
   - `jobs.failure` gains `build_failed`, `limit_exceeded` and
     `output_rejected` (rule 18); the change feed's kinds are unchanged;
   - `job_diagnostics`: the rows of rule 13.
10. **One build per revision and profile, the first one kept.** At most one
    build exists for an artifact, revision and profile. A later success for
    the same three publishes nothing new: its job ends `succeeded` and names
    the build that already exists, so a page served from that build never
    sees its files change.
11. **Publication is one transaction with the job's success.** The blobs, the
    build rows and the job's terminal state are written in the transaction
    that checks the binding of 0042 rule 7 again. A failed check writes none
    of them, so no build exists without its job's success and no blob is
    added outside it. 0057 rule 12 (counts only) is amended for `react-v1`.
12. **Reference rules and retention.** `build_files.digest` joins every
    predicate that keeps a blob (`remove_files`, `count_unreferenced`,
    `remove_unreferenced_content`) and the test that no binary column can
    hide a content digest. Purge deletes the artifact's builds and its jobs'
    diagnostics in its transaction. Trash keeps them and serves none. Backup
    copies them; restore keeps them, bound to the same revisions. An
    artifact keeps at most eight builds: the publishing transaction removes
    the oldest by publication time, never the latest build of rule 15 and
    never the one it publishes. Removed builds' blobs follow the existing
    reclamation.
13. **Structured, bounded diagnostics.** This amends 0030 rules 9 and 11 for
    `react-v1` only. A `build_failed` job keeps at most 20 diagnostics, each
    a package path, a line, a column and a message of at most 512 bytes:
    valid UTF-8 (invalid bytes replaced), no control character, and the
    `/input/` prefix rewritten to the package-relative path. The bundler's
    quoted source excerpt is dropped, but a message can still name source
    text (an import specifier, an identifier), so diagnostics are read only
    by whoever may read the job's revision, through the job, and there is no
    log endpoint. Only the newest failed `react-v1` job of an artifact keeps
    diagnostics: when another one ends, the earlier rows are deleted in that
    transaction. A succeeded job keeps none.

### Preview selection

14. **A build preview per revision.** The preview origin gains `/b/{ticket}/`.
    A build ticket is bound to one artifact, one revision and one profile,
    and serves only that build's files under the existing CSP sandbox. It
    is issued by the existing management preview operation given a build
    target, and lives in the same ticket registry, so token revocation,
    actor disable and trash revoke it as they revoke a source ticket. The
    API, MCP and the CLI all issue it. A target revision without a build is
    refused at issue.
15. **"Latest" is computed, not stored.** The latest build of an artifact for
    a profile is its build with the highest revision number. A late or
    older result can only add a build for its own revision, so it never
    replaces a newer one (0042 rule 7), and consecutive saves keep the last
    good build until a newer one succeeds.
16. **Head status.** When the head revision has no build, the reads report
    the state of the newest `react-v1` job for the head by enqueue order:
    `pending` (queued, retry scheduled or running), `failed` with that job's
    failure and diagnostics, or `none`. Older jobs for the head are listed
    with the job reads but do not set the status.

### The public kind

17. **`run_profile` is public with `profile: "react-v1"`.** The existing
    `POST /v1/artifacts/{id}/jobs` accepts it, pinning a target revision and
    taking an idempotency key as 0030 rule 2 does. There is no base-revision
    precondition, since the job writes no source: any revision of the
    artifact can be built, including one older than the head. The open-job
    bound applies. The job reads, the change feed, MCP and the CLI show the
    kind for a known profile. An enqueue of a profile this build does not
    know answers `profile_unknown`. R03 enqueues only on request; it does
    not build on every save and does not coalesce.
18. **The failure vocabulary.** None of these is retried:
    - `build_failed`: the bundler exited non-zero (with diagnostics), or the
      filter of rule 3 ended it (with one fixed diagnostic);
    - `limit_exceeded`: a limit of 0042 rule 5 ended the attempt;
    - `output_rejected`: rule 8 refused the result.

    `execution_unavailable`, cleanup failures and lease loss keep their
    meaning. R03bb extends the provider's verdict so these cases are told
    apart and the bundler's stderr is handed to the worker within the 1 MiB
    log bound; today all three reach `internal`.

### The operator's side

19. **A packaging script, not packaged bytes.** The repository holds a lock
    list and a script that an operator runs as root for each server
    release. The lock list names, for the bundler and each package, its
    version, download URL, SHA-256 and licence. The script assembles the
    whole root the provider admits, as `scripts/enforcement/prepare.sh` does
    for the fixture: the pinned launcher and its licence, the server binary
    as the bootstrap, the libraries that run inside the sandbox (the
    launcher runs on the host and loads the host's), the bundler, the packages,
    the licences and the manifest, with the owner and modes the provider
    checks, and the AppArmor launcher profile the supported host needs for
    unprivileged user namespaces. The root's path carries a version that the
    server build names (`/opt/artifree/tools/<version>`, replacing the fixed
    `v1`); since the bootstrap is the server binary, each server release
    names a new version, and the script refuses to write over an existing
    one (0042 rule 3: no in-place replacement). The enforcement job builds
    its root with the same script. Nothing of the root is committed, and no
    download runs in a worker or a build.
20. **A build never waits forever.** On a known target, a `react-v1` job
    that no execution worker has claimed within ten minutes of its enqueue
    ends as `execution_unavailable` while no execution worker is ready.
    Every worker's claim loop ends it, so the trusted worker, which always
    runs, does. A job that has an attempt is governed by 0057 and is not
    ended by this rule, and neither is a job whose artifact is in the
    trash or whose actor is disabled. `GET /v1/background` reports whether
    an execution worker is ready, so the web and the CLI can say why builds
    do not start.
21. **A supported host.** When R03f lands, Ubuntu 24.04 on x86_64, prepared
    by the script of rule 19, is the supported host. The operator
    documentation describes that preparation and the second worker unit
    (0056 rule 6). Other hosts are not supported.

### Slices

22. R03 is delivered in this order. Each slice passes `npm run check` and
    `npm run build`; slices that touch the worker also pass the enforcement
    job, and R03e passes Chromium and Firefox smoke. Until R03f, R03bcbb to
    R03e run `react-v1` only in the build with `enforcement-fixture`, and a
    release build knows no profile: before R03da its `run_profile` jobs stay
    internal (0057 rule 2), and from R03da its enqueue of `react-v1` is
    refused with `profile_unknown` (rule 17).

| Slice   | What it adds                                                                                                                                                                                                                                                                                                                                                                                                                                   | Proven where                                                                                                                                                                                                                                                                                                                                                               |
| ------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R03a    | Rule 6: the media types, the SDK mirror, the fixtures, `package_rules` and the contract bump.                                                                                                                                                                                                                                                                                                                                                  | Unit and contract tests on any host; a `.tsx` file is accepted and served as text; mutations of the new types.                                                                                                                                                                                                                                                             |
| R03ba   | The provider half of rules 1 and 3: a profile table that holds each profile's identity, payload, argv, environment, filter and manifest bytes, used at every site that names the fixture today; the fixture becomes its one entry, with no change in behavior.                                                                                                                                                                                 | Unit tests on any host for the table and each selection site; the enforcement job unchanged.                                                                                                                                                                                                                                                                               |
| R03bb   | The provider half of rule 18: verdicts that tell a non-zero payload exit, a limit that ends the attempt (the wall time, the memory and the log bound of 0042 rule 5) and a filter kill apart, and the hand-off of the payload's stderr within the 1 MiB log bound. A limit the payload sees as a failed call (tasks, descriptors, the scratch) shows as its non-zero exit. The worker still ends every such failure as `internal` until R03cb. | The enforcement job: the fixture exits non-zero, runs past the wall time, exceeds the memory and the log bound, and makes a forbidden call, and each gives its own verdict and the bounded stderr.                                                                                                                                                                         |
| R03bca  | Rule 19 without the bundler: the versioned root path and AppArmor profile name, the packaging script that `prepare.sh` then uses, its refusal to write over an existing root, and the provider's check of the owner and modes of the root, its entries and its ancestors. The fixture's root is the only one it builds.                                                                                                                        | The enforcement job: the script builds the fixture's root at the versioned path and refuses an existing one; the refusals of a modified, an extra and a missing file hold for that root; and a root with another owner, a group- or other-writable entry, a set-ID bit or an unprotected ancestor is refused.                                                              |
| R03bcba | The rest of rule 19 for the bundler: the lock list (version, download URL, SHA-256 and licence of the bundler and each package), and the packaging script that downloads, checks and unpacks them into the root. A test template, and the in-sandbox measurement of rule 3's calls with a measuring filter in the build with `enforcement-fixture`. No profile is added.                                                                       | The enforcement job: the script's root holds the bundler and the packages within the manifest's limits; a wrong SHA-256 is refused before anything is written to the root; a changed package file is refused as a modified root. The template, a bad import and a larger input build under the measuring filter with no kill, and their calls are recorded as an artifact. |
| R03bcbb | Rules 1 to 4 in the build with `enforcement-fixture`: `react-v1` with the filter measured in R03bcba, its argv and environment. Its note in 0057 says that the feature now holds a second profile.                                                                                                                                                                                                                                             | The enforcement job: the template builds within every limit, a bad import fails with its bounded stderr, a forbidden call is killed, and a modified root is refused.                                                                                                                                                                                                       |
| R03ca   | Rule 9 and the store half of rules 10 to 13 and 18: schema 15 with the new failure values, the publishing operation with the job's success, the first build kept, the eight-build bound with the latest-build query that guards it, the reference predicates, purge, trash, backup and restore, and the newest failed job's diagnostics.                                                                                                       | SQLite tests on any host for each transaction, reference and retention rule, the first-build-kept rule, the latest build never pruned, purge, trash (builds kept and served by none), backup and restore, and a 25 MiB publication within the worker's lease.                                                                                                              |
| R03cb   | Rules 7 and 8, and the worker half of rules 11, 13 and 18: composition, the split of the bundler's messages into diagnostics, the failure names (including resolver configuration as `build_failed`) and the publication through R03ca.                                                                                                                                                                                                        | Unit tests on any host for composition and the message split; the enforcement job for a published build, a `build_failed` with its diagnostics, a filter kill as `build_failed` with its fixed diagnostic, resolver configuration as `build_failed`, a `limit_exceeded` and an `output_rejected`.                                                                          |
| R03cc   | Rules 14 to 16 below the wire, and the read rule of 13: the build target of the management preview operation, build tickets and their revocation, the `/b/{ticket}/` route, and the head status on R03ca's latest-build query.                                                                                                                                                                                                                 | SQLite and service tests on any host for the head status, ticket revocation and the refusal of diagnostics to a caller who may not read the revision; HTTP tests for the `/b/` route under the CSP sandbox; the enforcement job for the preview of a published build.                                                                                                      |
| R03cda  | Rule 20's readiness: schema 16 with `execution_workers`, the execution worker's row written at capacity and at each lease renewal (at most once a minute), deleted at a refusal and emptied at a restore, and `execution.ready` in `GET /v1/background`, with contract 0.29.0.                                                                                                                                                                 | Store and core tests with the injected clock for the row at capacity and at a renewal, the one-minute rewrite, the refusal, the pruning, the window's edges and the restore; contract and API tests for the readiness; the enforcement job, where a worker with capacity makes it ready.                                                                                   |
| R03cdb  | Rule 20's deadline, after R03cda: every worker's claim loop ends a known build profile's queued job with no attempt row, outside the trash and of an enabled actor, ten minutes after its enqueue as `execution_unavailable` while no execution worker is ready.                                                                                                                                                                               | Tests on any host with the injected clock for the deadline's edge, a ready worker, a job with an attempt row, a job in the trash, a disabled user's job, a profile that is not a build profile, an execution worker's loop, a job claimed first, a worker row seen after now and a worker's start-up wait; the enforcement job unchanged.                                  |
| R03da   | Rule 17 and the wire half of 18: the public kind, `profile`, `profile_unknown`, the new failures, the build counts and the diagnostics on the job reads, contract 0.30.0, in the API, MCP, the client and the CLI.                                                                                                                                                                                                                             | Contract, API, MCP, client and CLI tests, including refusals and the open-job bound; web component tests and Chromium and Firefox smoke for the new names.                                                                                                                                                                                                                 |
| R03db   | The wire half of rules 14 and 16: the build preview route and the head status read, contract 0.31.0, in the API, MCP, the client and the CLI.                                                                                                                                                                                                                                                                                                  | Contract, HTTP, MCP, client and CLI tests for both calls and their refusals.                                                                                                                                                                                                                                                                                               |
| R03ea   | The build in the Jobs tab: the build action, the head status and its diagnostics, and the build rows, after the R03e split note.                                                                                                                                                                                                                                                                                                               | Component tests, and Chromium and Firefox smoke for the request, the pending and failed states and the refusal.                                                                                                                                                                                                                                                            |
| R03eb   | The build preview: the choice between files and build, and the "latest" notice, after R03ea.                                                                                                                                                                                                                                                                                                                                                   | Component tests, and Chromium and Firefox smoke for the choice, the notice and the refusal. No smoke host runs a profile, so a successful build and its `/b/` preview are proven by component tests and by the enforcement job at the HTTP level.                                                                                                                          |
| R03f    | Rules 5 and 21: the release build for x86_64 Linux compiles the launch path and knows `react-v1`, and the operator documentation and the supported-host statement.                                                                                                                                                                                                                                                                             | Release-archive and clean-install checks of the launch path; the enforcement job runs the release binary's worker; review of the documentation.                                                                                                                                                                                                                            |

### What a slice still has to settle

23. These are assumptions the named slice must prove, and amend this record
    if they fail:
    - R03bcba: that the bundler's system calls inside the sandbox, on the
      template, a bad import and a larger input, include no call that the
      public syscall mapping refuses.
    - R03bcbb: that the bundler builds the template under the limits of 0042
      rule 5 with one CPU and 64 tasks.
    - R03bcba: that the bundler resolves the preinstalled packages through the
      profile's environment alone, without a configuration file in the
      revision.
    - R03cb: that the bundler's messages can be split into rule 13's fields.
    - R03ca: that a 25 MiB publication fits in one SQLite transaction within
      the worker's lease.

## Alternatives considered

- **A Node.js runtime with a JavaScript bundler.** Rejected for R03: a
  larger root, more system calls and a loader to package, with no gain for
  one fixed template.
- **Installing packages per build.** Rejected: 0042 rule 3 forbids downloads
  at run time and needs its own specification with networking.
- **Committing the tool root or its packages.** Rejected: vendor bundles do
  not belong in the repository (AGENTS.md, provenance).
- **A stored "current build" pointer.** Rejected: a computed latest needs no
  update order, so a late result cannot win a race.
- **Replacing a revision's build with a later success.** Rejected: an open
  build preview would change under its reader, and a slower earlier job
  would win.
- **Free-text build logs with the job.** Rejected: 0030 keeps job failures
  free of text; structured, bounded rows for the newest failure are the
  smallest amendment that lets a user fix a build.
- **Generating `index.html` in the worker.** Rejected for R03: the template
  owns its page, and checking one fixed script line is enough to keep a
  build from referencing a missing file.
- **Building on every save.** Deferred: it needs coalescing and a policy for
  the open-job bound. R03 builds on request.
- **Serving the build under `/p/`.** Rejected: a revision's source and its
  build are different packages, and one ticket must name one.

## Consequences

- A release server for x86_64 Linux can run untrusted code for the first
  time, on a prepared host with an operator-installed root and a second
  worker unit. Each server release needs its root prepared again.
- The store keeps build bytes beside revision bytes, under the same
  reclamation, and a purge removes them.
- Jobs may hold bounded, structured build diagnostics, the first job text
  derived from source.
- The contract gains source media types, a public kind, `profile_unknown`,
  three failures, build tickets and a build preview route.
- T01b can add the first React template on top of R03.
