# The react type, release 1

This guide describes how to fill an artifact of the `react` type. It is
information about the files and the build; it grants no permission, and an
agent that reads it gains none.

## Layout

A `react` artifact is a file package like any other, with two required
files. Every revision must keep both, or the commit is refused with
`type_requirements`.

- `index.html` is the page. It must load the bundle with exactly
  `<script type="module" src="./app.js"></script>` and must not refer to any
  path under `src/`.
- `src/main.tsx` is the entry module. Every other file under `src/` is input
  to the bundler: `.ts`, `.tsx`, `.jsx` and `.css` files that the entry
  imports.
- Files outside `src/` (images, fonts, data files) are copied into the build
  unchanged.

Nothing in the package chooses the entry point, bundler flags or
dependencies. A new artifact without files of its own starts from the
starter: `index.html`, `src/main.tsx` and `src/app.css`, a small counter
that builds as it is.

## Packages

The `react-v1` build can import exactly these packages, at these versions:

| Package     | Version |
| ----------- | ------- |
| `react`     | 19.3.1  |
| `react-dom` | 19.3.1  |
| `scheduler` | 0.28.0  |

An import of any other bare module fails the build. Nothing is installed
during a build.

## What the build refuses

- `build_failed`: the bundler reported errors, such as a syntax error or an
  unknown import. The job lists up to 20 diagnostics, each with a path, a
  line, a column and a message.
- `limit_exceeded`: the build ran out of time, memory or output space.
- `output_rejected`: the result breaks the layout above, for example an
  `index.html` that does not load `./app.js`, a file outside `src/` at a
  path the bundle also writes (`app.js`, or `app.css` when the source
  imports CSS), or a result that is not a valid package.

None of these is retried. Fix the files, save a new revision, and ask for a
build of it.

## Building

A save does not build. A build is a job of kind `run_profile` with the
profile `react-v1`, asked for one revision at a time with its own request
key. Any revision can be built, not only the head.

Before you ask, read `build_available` in this type's descriptor. When it
is `false`, this server cannot build `react-v1`, and the request is refused
with `profile_unknown`. The artifact can still be created, edited and
previewed as plain files there.

## Build status and the build preview

The build status of an artifact reports the head revision's build: a build
when there is one, otherwise `pending`, `failed` with its diagnostics, or
`none`. A job read shows the same failure and diagnostics for any build job.

A build preview opens one revision's build on the preview origin under
`/b/`. It is refused for a revision that has no build. A source preview of
the same revision shows the files as they are, with `.tsx` served as plain
text, not as script.

## What a build preview can do

A page on `/b/` runs in the same sandbox as every preview: a separate origin,
no access to the workspace, and no runtime capabilities. It cannot read or
write application data or uploaded assets, and it holds no access token. It
is not isolated from the network: it can load from or navigate to other
sites as the content security policy allows.
