# 0058: React builds and their preview

Date: 2026-11-14. Status: **accepted; R03a (the package rules), R03ba
(the profile table), R03bb (the provider's verdicts), R03bca (the
versioned, packaged and protected tool root), R03bcba (the lock list, the
packaged bundler and its measurement) and R03bcbb (the `react-v1` profile in
the build with `enforcement-fixture`) are implemented, R03c to R03f are
not**. Roadmap: R03.

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
> exit, as it did before this slice, and R03c must keep that in mind when it
> names these failures. The launch hands the stub's stderr to the worker on
> every path on which the payload started, within the 1 MiB bound it shares
> with stdout, and none when the payload did not start, so the bootstrap's
> own diagnostics never pass for the payload's. The worker
> prints only its length beside the provider's word, and still fails each such
> job as `internal` until R03c.

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
> which is rule 1's "no source maps". The enforcement job (part 16) shows:
>
> - the template builds and exits 0 in an attempt group that reads back the
>   limits of 0042 rule 5. Its output is collected within the output limits
>   and refused only for its missing `index.html`, which R03c's composition
>   adds (rule 8). This proves rule 23's assumption for R03bcbb, so 0042 is
>   not amended;
> - a bad import exits 1 with the one message that names it;
> - a call outside the filter is killed with exactly its record. The bundler
>   cannot be made to make such a call, so an entry with no store name runs
>   the fixture's payload under `react-v1`'s filter, and its thread calls
>   `unshare`;
> - a changed file of the `react` package refuses the `react-v1` attempt as a
>   modified root. Admission checks the whole root before any profile matters,
>   so this shows the root check that `react-v1` runs behind, as part 5 does
>   for the fixture.

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
> provider's output collection now also accepts these extensions, which R03c's
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
   under `src/`; R03c's composition checks both and refuses the build
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

9. **Schema 15.** It adds two tables and rebuilds `jobs` once, so that R03c
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
    ends as `execution_unavailable`; the trusted worker, which always runs,
    ends it at its claim loop. A job that has an attempt is governed by
    0057 and is not ended by this rule. `GET /v1/background` reports whether
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
    release build knows no profile: before R03d its `run_profile` jobs stay
    internal (0057 rule 2), and from R03d its enqueue of `react-v1` is
    refused with `profile_unknown` (rule 17).

| Slice   | What it adds                                                                                                                                                                                                                                                                                                                                                                                                                                  | Proven where                                                                                                                                                                                                                                                                                                                                                               |
| ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R03a    | Rule 6: the media types, the SDK mirror, the fixtures, `package_rules` and the contract bump.                                                                                                                                                                                                                                                                                                                                                 | Unit and contract tests on any host; a `.tsx` file is accepted and served as text; mutations of the new types.                                                                                                                                                                                                                                                             |
| R03ba   | The provider half of rules 1 and 3: a profile table that holds each profile's identity, payload, argv, environment, filter and manifest bytes, used at every site that names the fixture today; the fixture becomes its one entry, with no change in behavior.                                                                                                                                                                                | Unit tests on any host for the table and each selection site; the enforcement job unchanged.                                                                                                                                                                                                                                                                               |
| R03bb   | The provider half of rule 18: verdicts that tell a non-zero payload exit, a limit that ends the attempt (the wall time, the memory and the log bound of 0042 rule 5) and a filter kill apart, and the hand-off of the payload's stderr within the 1 MiB log bound. A limit the payload sees as a failed call (tasks, descriptors, the scratch) shows as its non-zero exit. The worker still ends every such failure as `internal` until R03c. | The enforcement job: the fixture exits non-zero, runs past the wall time, exceeds the memory and the log bound, and makes a forbidden call, and each gives its own verdict and the bounded stderr.                                                                                                                                                                         |
| R03bca  | Rule 19 without the bundler: the versioned root path and AppArmor profile name, the packaging script that `prepare.sh` then uses, its refusal to write over an existing root, and the provider's check of the owner and modes of the root, its entries and its ancestors. The fixture's root is the only one it builds.                                                                                                                       | The enforcement job: the script builds the fixture's root at the versioned path and refuses an existing one; the refusals of a modified, an extra and a missing file hold for that root; and a root with another owner, a group- or other-writable entry, a set-ID bit or an unprotected ancestor is refused.                                                              |
| R03bcba | The rest of rule 19 for the bundler: the lock list (version, download URL, SHA-256 and licence of the bundler and each package), and the packaging script that downloads, checks and unpacks them into the root. A test template, and the in-sandbox measurement of rule 3's calls with a measuring filter in the build with `enforcement-fixture`. No profile is added.                                                                      | The enforcement job: the script's root holds the bundler and the packages within the manifest's limits; a wrong SHA-256 is refused before anything is written to the root; a changed package file is refused as a modified root. The template, a bad import and a larger input build under the measuring filter with no kill, and their calls are recorded as an artifact. |
| R03bcbb | Rules 1 to 4 in the build with `enforcement-fixture`: `react-v1` with the filter measured in R03bcba, its argv and environment. Its note in 0057 says that the feature now holds a second profile.                                                                                                                                                                                                                                            | The enforcement job: the template builds within every limit, a bad import fails with its bounded stderr, a forbidden call is killed, and a modified root is refused.                                                                                                                                                                                                       |
| R03c    | Rules 7 to 16, 20 and the store half of 18: composition, schema 15, publication, references, retention, diagnostics, build tickets, the `/b/` route, head status and the claim deadline.                                                                                                                                                                                                                                                      | SQLite tests on any host for each transaction, reference and retention rule, the first-build-kept rule, purge, backup, restore and the deadline; the enforcement job for a published build, a `build_failed` with diagnostics and its preview.                                                                                                                             |
| R03d    | Rule 17 and the wire half of 18: the public kind, `profile_unknown`, the contract, MCP and the CLI, including build tickets.                                                                                                                                                                                                                                                                                                                  | Contract, API, MCP and CLI tests, including refusals and the open-job bound.                                                                                                                                                                                                                                                                                               |
| R03e    | The web: a build action, head status and diagnostics, and the build preview with its "latest" notice.                                                                                                                                                                                                                                                                                                                                         | Component tests, and Chromium and Firefox smoke for the request, the pending and failed states and the refusal. No smoke host runs a profile, so a successful build and its `/b/` preview are proven by component tests and by the enforcement job at the HTTP level.                                                                                                      |
| R03f    | Rules 5 and 21: the release build for x86_64 Linux compiles the launch path and knows `react-v1`, and the operator documentation and the supported-host statement.                                                                                                                                                                                                                                                                            | Release-archive and clean-install checks of the launch path; the enforcement job runs the release binary's worker; review of the documentation.                                                                                                                                                                                                                            |

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
    - R03c: that the bundler's messages can be split into rule 13's fields.
    - R03c: that a 25 MiB publication fits in one SQLite transaction within
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
