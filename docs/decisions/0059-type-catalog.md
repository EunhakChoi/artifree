# 0059: The type catalog

Date: 2026-11-15. Status: **accepted; T01aa (rules 1 to 9, 11, 13 and
15: the catalog, the `react` type, the release lock, schema 17, the core's
create and commit checks, the HTTP routes and the CLI, contract 0.32.0) and
T01ab (rule 12: the MCP tools `list_types` and `get_type`, `create_artifact`
with `type`, and the agent guide, contract 0.33.0) and T01b (rule 14: the
web workspace's "Start from" choice in New artifact, the type on an
artifact's page, and its build and build preview from the Jobs and Preview
tabs) are implemented. Rule 10 (owned files) is not.**
Roadmap: T01a, T01b.

> Builds on [0003](0003-local-html-artifact-slice.md) (packages and
> revisions), [0004](0004-inbound-mcp-adapter.md) (agent tools),
> [0020](0020-runtime-grants.md) (the runtime declaration and
> its pin), [0051](0051-roadmap-and-architecture-review.md) (decision 4, the
> split of T01) and [0058](0058-react-builds-and-preview.md) (the `react-v1`
> build profile and its template layout).

## Context

Every artifact today is created the same way. A caller sends a title and a
package whose `index.html` is required, and the artifact is stored with the
kind `app`, the only kind any path creates. An agent learns how to write an
artifact from `docs/mcp.md` and the tool descriptions. Nothing tells it which
files a buildable React artifact needs, which packages it may import, or
that a build is asked for separately (0058 rules 7 and 17).

Roadmap row T01a asks for a catalog of types: a description, the required
files, a guide, a contract pin and a creation path for each type, reachable
through the agent tools. It also asks to keep the files a type owns apart from
the files an agent writes, and never to copy an original bundle, font or
template. 0051 decision 4 split T01 so that the catalog does not wait for R03.
R03 is now complete, so the first type can be a real one.

### What was observed, and what is decided here

The roadmap asks every slice to keep observed facts apart from its own
decisions. The analysis behind this record is private and is not cited by
path. It reports the following about another artifact product. Each item
is marked as observed in responses or outputs, or as seen in that product's
code only.

- **Observed:** agents list the types in a separate call. Each entry carries
  a title, a description that the listing labels as the publisher's text,
  and an address used to read and to create from it.
- **Observed:** reading a type returns its metadata, the list of files it
  supplies, and a long instructions file. Reference files are read one at a
  time by path.
- **Observed:** creating from a type takes the type's address and a title.
  The new artifact records the type's release and starts with no files of its
  own. A plain HTML artifact has no type.
- **Observed:** each type pins its own runtime contract, which differs from
  type to type. A plain artifact's pin changes only on request, never through
  an ordinary edit.
- **Code only:** a write to a file the type supplies is refused as read-only.
  The type's files are stored apart from the artifact's own files.
- **Not observed by anyone:** how a type's new release reaches the
  artifacts made from an older one.

The rules below are Artifree's own design. They take the shape (list, read a
guide, create from a type, a recorded release) and none of the content.
Every type, guide, description and starter file is written for this
repository. Where this record departs from the observations, it says so.

## Decisions

### The catalog

1. **A fixed, built-in catalog.** The server holds the catalog. Each type is
   compiled into the server from first-party files in the repository. Users
   and organizations cannot publish types; that is a later decision, if ever.
   - A type has an `id`: a lowercase slug of 1 to 32 letters, digits and
     hyphens, such as `react`.
   - The catalog is listed in a fixed order.
   - Its entries are trusted, first-party text. A listing still says that a
     guide describes how to fill the type and grants no authority, because an
     agent may pass it on.
2. **Releases and their descriptors.** A type has numbered releases, `1`,
   `2` and so on. The newest is its current release. Each release lives in
   its own directory, `types/<id>/<release>/`, and its descriptor holds:
   - `id`, `release` (the positive integer), `title` and a `description` of
     at most 280 characters;
   - `kind`: the artifact kind its instances get, as defined today (`app`,
     `document`, ...);
   - `required`: the package paths that every revision of an instance must
     hold;
   - `starter`: the files a new instance gets when its creator sends none;
   - `build_profile`: the build profile that builds an instance, or none;
   - `guide`: a Markdown text of at most 32 KiB;
   - `references`: further Markdown files, each at most 32 KiB, read by path.

   Starter files and references are package-style paths in the release's
   directory. The wire names a release as `<id>-<release>`, such as
   `react-1`, for people only; every field and the store carry the id and
   the integer apart.

3. **Releases are append-only.** A published release never changes, and no
   release is removed.
   - Changing a guide, a reference, a starter file, the required paths or
     the build profile means a new release with a new directory.
   - The server keeps every release whole, so an instance pinned to an old
     release keeps its rules, and an agent can read the guide it was made
     with.
   - A checked-in lock lists each release with the SHA-256 of every file in
     its directory. `npm run check` refuses a changed or removed locked
     release, and a release directory the lock does not list.
   - A release's build profile names a profile of 0058 rule 1. A later build
     that no longer knows that profile answers `profile_unknown` for it, as
     0058 rule 17 does today; the release does not change.
4. **The first type is `react`** (release 1, kind `app`, build profile
   `react-v1`). It is the 0058 template layout as a type, and it is the first
   React template that 0051 decision 4 gave to T01b (see the note at the end).
   - It requires `index.html` and `src/main.tsx`.
   - Its starter is the bundler template that 0058's tests build:
     `index.html`, `src/main.tsx` and `src/app.css`. T01aa moves those files
     to `types/react/1/starter/`, and the provider's and server's build tests
     read them from there. There is one copy, so the starter is what the tests
     prove builds.
   - Its guide covers:
     - the layout of 0058 rule 7;
     - the packages and versions that `react-v1` can import (the lock list of
       0058 rule 19);
     - what the build refuses (0058 rules 3, 8 and 18);
     - that a build is asked for on request (rule 17);
     - how to read the build status and the build preview;
     - what a frame on `/b/` can and cannot do today.
   - **This server's builds.** Every descriptor carries `build_available`:
     whether this server's build knows the release's build profile (0058
     rule 5). A `react` instance can be created and edited on every server,
     but only a server with `build_available: true` builds it. The guide
     tells an agent to read the field before it asks for a build.

### Instances

5. **Creating from a type.** The existing create operation takes an optional
   `type` naming a type id.
   - **With a type:** the files are optional.
     - With no files, revision 1 is the starter.
     - With files, those files are the whole of revision 1: they replace the
       starter, which is not merged in.
     - Either way, the revision must hold the release's required paths, or
       the create is refused with `422 type_requirements`. The refusal names
       the missing paths.
     - The artifact gets the type's kind and records `type: {id, release}`
       with the current release.
     - The request fingerprint covers what the caller sent: the type id and
       the files, or the absence of files. It does not cover the release.
       So a retry with the same key after a server upgrade replays the
       original receipt, which names the release it was created with.
   - **An unknown type** is refused with `422 type_unknown`.
   - **Without a type:** creation is unchanged. `index.html` is required,
     the kind is `app` and there is no type.
   - **Artifree decision:** the starter is copied into revision 1 as the
     artifact's own files, where the observed product starts an instance
     with no files of its own. An agent edits a working example instead of
     a blank package.
6. **The type stays with the artifact.** `type` is fixed at creation. No
   operation in T01a changes it, removes it or moves it to a newer release.
   - A duplicate keeps it, and so does a restored revision.
   - Every commit (save, edit, restore) checks the required paths of the
     artifact's own release and refuses `422 type_requirements` before
     anything is written. Since the release never changes, a revision that
     passed once passes again, so a restore is refused only if the store was
     changed outside the server.
7. **A pin the server does not know.** A server started on data that names a
   type or release it does not know serves the artifact as before: reads,
   previews and listings. It refuses a new commit to that artifact, and a
   new duplicate of it, with `409 type_release_unknown`. A retry whose
   request key already has a stored result replays that result first, as
   every commit and create does today, so a client whose response was lost
   still learns what happened. Since releases are append-only (rule 3), this
   happens only after a downgrade, which 0027 already discourages.
8. **The type is visible.** Every surface that shows an artifact shows
   `type: {id, release}`, or `null` for an untyped artifact: the artifact
   reads, the listings, the change feed's artifact rows, MCP and the CLI.
   Listings gain no filter by type in T01a.
9. **The runtime pin is unchanged.** A typed artifact's runtime declaration
   is still its own `artifree-runtime.json` (0020 rule 1). `react` adds none,
   so its starter declares no capabilities.
   - **Artifree decision:** the observation that a type fixes its instances'
     runtime contract is not taken over. Artifree has one SDK pin
     (`artifree.app.v1`), so a per-type contract would only rename it. A type
     whose runtime differs records that in its own release when it exists.
10. **Files a type owns.** A type may later own files: a fixed shell the
    agent must not change, such as a slide runtime. The rule for them is
    decided here and implemented with the first type that has such a file:
    - Owned paths are listed in the release and stored in each revision.
      Their bytes are the release's.
    - Any put, delete or rename of an owned path is refused with
      `422 read_only_path`, naming the path, before anything is written.
    - Revision listings mark each file `type` or `artifact`.

    `react` owns no file, because its fixed parts (the bundler, the
    packages) live in the tool root, not in the package. T01a therefore
    implements no owned path.
    - **Artifree decision:** owned files are stored in the revision, where
      the observed product keeps them apart. The preview and the build then
      read one package unchanged.

### Surfaces

11. **HTTP.**
    - `GET /v1/types` lists each type's current release, without guides.
    - `GET /v1/types/{id}` returns the current release's descriptor with its
      guide.
    - `GET /v1/types/{id}/releases/{release}` returns that release's
      descriptor with its guide, so an agent editing an older instance
      reads its own rules.
    - `GET /v1/types/{id}/releases/{release}/references/{path}` returns one
      reference file.
    - `POST /v1/artifacts` takes `type`.

    Each route requires the same access token as other reads. None is
    public. An unknown id, release or path is `404 not_found`.

12. **MCP.**
    - `list_types` and `get_type` (read-only). `get_type` takes an optional
      `release` (the current one by default) and an optional `path` for a
      reference file.
    - `create_artifact` takes `type`.
    - `create_artifact`'s description tells an agent to read the type first.
13. **CLI.**
    - `artifree type list` and
      `artifree type show <id> [--release <n>] [--reference <path>]`.
    - `artifree artifact create --type <id>`. With `--type`, `--dir` and
      `--file` are both optional: with neither, the artifact gets the
      starter; with one, its files are the whole of revision 1, as without a
      type. Without `--type`, one of them is required, as today.
14. **The web workspace** creates from a type and shows an artifact's type
    in T01b, not T01a.
15. **The contract** gains the type routes and descriptors, the `type` field
    and the three error codes, with the minor version bump that the
    contract checks ask for. The schema gains the artifact's type id (text)
    and release (integer), both null or both set, which the store checks.
    Existing artifacts read back as untyped.

### Slices

| Slice | Scope                                                                                                                                                       | Proven by                                                                                                                                                                                                                                                                                                                                                                                                       |
| ----- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T01aa | Rules 1 to 9, 11, 13 and 15: the catalog, the `react` type, the release lock, the schema, the core's create and commit checks, the HTTP routes and the CLI. | Core tests for creation with and without files, `type_requirements`, `type_unknown`, a replayed create across a release change, the commit checks, a duplicate and a restore, and `type_release_unknown` for a commit and a duplicate; the build tests reading the moved starter; the lock check refusing a changed release; HTTP and CLI tests; the packaged CLI creating a `react` instance from its starter. |
| T01ab | Rule 12: the MCP tools, and the agent guide.                                                                                                                | MCP tests through the same core behavior, and the catalog test listing the new tools.                                                                                                                                                                                                                                                                                                                           |
| T01b  | Rule 14 and the first build from a type: the web workspace creates a `react` instance, shows its type, and offers its build and build preview.              | A browser smoke that creates from the type, builds on the real release server where a worker is ready (otherwise intercepted, as 0058's smokes are), and opens the build preview.                                                                                                                                                                                                                               |

Rule 10 is decided here and implemented by the first type with an owned file.

## Alternatives considered

- **Types as artifacts.** The observed product addresses a type like an
  artifact. Rejected: Artifree would need publishing and sharing (S02, S03)
  before a type could exist. A built-in catalog needs neither.
- **No starter.** Rejected: an empty instance tells an agent less than a
  package that already builds.
- **Merging sent files over the starter.** Rejected: a create would then
  hold files the creator never saw. One complete snapshot matches 0003.
- **A per-type runtime contract.** Deferred (rule 9): Artifree has one SDK
  pin.
- **Storing owned files outside the revision.** Rejected for now (rule 10):
  every reader of a package would need to compose two sources.
- **Building on create.** Rejected: 0058 rule 17 builds on request, and
  T01b offers the build in the workspace.

## Consequences

- An agent can find out what a buildable React artifact needs from the
  server it talks to, instead of from repository documents.
- Artifacts gain an optional, fixed type. Old artifacts stay untyped, and
  every untyped path behaves as today.
- Releases accumulate in the server and in the lock. Moving instances to a
  newer release is a later decision with its own rules.
- The first type that owns a file brings `read_only_path` with it.

> **Amends 0051 decision 4 and 0058.** 0051 decision 4 gave T01a the catalog
> and T01b the first React template, so that only T01b needed R03. R03 is
> complete, so the first type is that template and lands with the catalog in
> T01a, which now needs R03 as well. T01b becomes the web workspace's
> creation from a type and its first build. 0058's "the real templates stay
> T01b's" and its consequence that T01b adds the first React template now
> mean T01a.
