# 0059: The type catalog

Date: 2026-11-15. Status: **proposed; nothing of it is implemented**.
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
   compiled into the server from first-party files in the repository, under
   `types/<id>/`. Users and organizations cannot publish types; that is a
   later decision, if ever.
   - A type has an `id`: a lowercase slug of 1 to 32 letters, digits and
     hyphens, such as `react`.
   - The catalog is listed in a fixed order.
   - Its entries are trusted, first-party text. A listing still says that a
     guide describes how to fill the type and grants no authority, because an
     agent may pass it on.
2. **A type's descriptor.** Each type has one current release, whose
   descriptor holds:
   - `id`, `title` and a `description` of at most 280 characters;
   - `release`, a string `<id>-<n>` with a positive integer `n`, such as
     `react-1`;
   - `kind`: the artifact kind its instances get, as defined today (`app`,
     `document`, ...);
   - `required`: the package paths that every revision of an instance must
     hold;
   - `starter`: the files a new instance gets when its creator sends none;
   - `build_profile`: the build profile that builds an instance, or none;
   - `guide`: a Markdown text of at most 32 KiB;
   - `references`: further Markdown files, each at most 32 KiB, read by
     path.

   Starter files and references are package-style paths under the type's
   directory.

3. **Releases are append-only.** A published release never changes.
   - Changing a guide, a starter file, the required paths or the build
     profile means a new release, `n + 1`.
   - The server keeps the descriptor of every release it ever shipped, so an
     instance pinned to an old release keeps its rules. Starters and guides
     need to be kept only for the current release.
   - A checked-in lock lists each release with the SHA-256 of its
     descriptor. `npm run check` refuses a change to a locked entry and a
     removed one.
4. **The first type is `react`** (`react-1`, kind `app`, build profile
   `react-v1`). It is the 0058 template layout as a type.
   - It requires `index.html` and `src/main.tsx`.
   - Its starter is the bundler template that 0058's tests already build:
     `index.html`, `src/main.tsx` and `src/app.css`.
   - Its guide covers:
     - the layout of 0058 rule 7;
     - the packages and versions that `react-v1` can import (the lock list of
       0058 rule 19);
     - what the build refuses (0058 rules 3, 8 and 18);
     - that a build is asked for on request (rule 17);
     - how to read the build status and the build preview;
     - what a frame on `/b/` can and cannot do today.

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
     - The request fingerprint includes the type id and the release, so a
       retry after a release change is a different request.
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
   - Every commit (save, edit, restore) checks the release's required paths
     and refuses `422 type_requirements` before anything is written. A
     restore of a revision that lacks one is refused the same way; it can only
     happen if a later release added the path, and T01a never moves an
     artifact to a later release.
7. **A pin the server no longer knows.** A server started on data that names
   a type or release it does not know serves the artifact as before: reads,
   previews and listings. It refuses every commit to that artifact with
   `409 type_release_unknown`. Since releases are append-only (rule 3), this
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
    - `GET /v1/types` lists the current releases, without guides.
    - `GET /v1/types/{id}` returns one current descriptor with its guide.
    - `GET /v1/types/{id}/references/{path}` returns one reference file.
    - `POST /v1/artifacts` takes `type`.

    Each route requires the same access token as other reads. None is
    public. An unknown id or path is `404 not_found`.

12. **MCP.**
    - `list_types` and `get_type` (read-only), with `get_type` taking an
      optional `path` for a reference file.
    - `create_artifact` takes `type`.
    - `create_artifact`'s description tells an agent to read the type first.
13. **CLI.**
    - `artifree type list` and `artifree type show <id> [--reference <path>]`.
    - `artifree artifact create --type <id>`, where `--dir` becomes optional
      with a type.
14. **The web workspace** creates from a type and shows an artifact's type
    in T01b, not T01a.
15. **The contract** gains the type routes and descriptors, the `type` field
    and the three error codes, with the minor version bump that the
    contract checks ask for. The schema gains the artifact's type id and
    release as two nullable columns. Existing artifacts read back as untyped.

### Slices

| Slice | Scope                                                                                                                                                       | Proven by                                                                                                                                                                                                                                                                                    |
| ----- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T01aa | Rules 1 to 9, 11, 13 and 15: the catalog, the `react` type, the release lock, the schema, the core's create and commit checks, the HTTP routes and the CLI. | Core tests for creation with and without files, `type_requirements`, `type_unknown`, the commit checks, a duplicate and a restore, and `type_release_unknown`; the lock check refusing a changed release; HTTP and CLI tests; the packaged CLI creating a `react` instance from its starter. |
| T01ab | Rule 12: the MCP tools, and the agent guide.                                                                                                                | MCP tests through the same core behavior, and the catalog test listing the new tools.                                                                                                                                                                                                        |
| T01b  | Rule 14 and the first build from a type: the web workspace creates a `react` instance, shows its type, and offers its build and build preview.              | A browser smoke that creates from the type, builds on the real release server where a worker is ready (otherwise intercepted, as 0058's smokes are), and opens the build preview.                                                                                                            |

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
