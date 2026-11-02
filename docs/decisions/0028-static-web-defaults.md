# 0028: Static web defaults and container settings

Date: 2026-11-02. Status: accepted. Builds on
[0022](0022-production-web-and-api-serving.md),
[0024](0024-https-proxy-and-remote-access.md), and
[0026](0026-container-image.md). Roadmap row: O03d. Independent Artifree design:
the private evidence index lists no original-product constraint on deployment
configuration for this row, so the choices below are Artifree's own.

## Context

O03b showed that two setups could only be declared with arguments: a container
(`--content-origin` is mandatory in practice, 0026 rule 5) and a web workspace
hosted apart from the API (`--allowed-origin` and `--frame-ancestors`, with the
API address typed or linked per tab). The roadmap row asks for a default server
in the static web build, environment equivalents for `--content-origin` and
`--frame-ancestors`, and a review of the CORS allow-list needing a restart.

While implementing it we found three things about the code as it stood:

- The web workspace used `?server=` and the Server URL field as raw strings. It
  checked nothing: an address with credentials made every request fail inside
  `fetch` with a message that did not say why, and a non-`http(s)` address was
  only refused by the browser.
- `normalize_origin` accepted `https://user:secret@host` and silently dropped
  the credentials.
- `--frame-ancestors` took any visible-ASCII text. An empty value produces an
  invalid directive that browsers ignore, which lets any page embed previews,
  and a `;` starts another directive in the preview policy.

## Decision

### 1. The static default is a build-time setting

`VITE_ARTIFREE_SERVER=<address> npm run build` compiles a default API address
into the web workspace. Without it nothing changes.

We chose build time over a runtime file such as `artifree-config.json` next to
`index.html`, for these reasons:

- A runtime file costs an extra request on every page load, an asynchronous
  start-up before the first render, and a decision about hosts that answer a
  missing file with `index.html`. Where the file is absent (the normal case:
  `artifree-server` serving its own build) the request is a 404, and Chromium
  logs every failed response as a console error; the smoke suite requires zero.
  A build-time value costs none of this.
- Static hosts already build the site (the guide tells operators to run
  `npm run build`, and hosts such as Vercel take the variable from project
  settings), so a rebuild is not an extra burden.
- A bad value can fail the build. The Vite config validates it with the page's
  own function and refuses the build, so a page that cannot connect is never
  published. A runtime file could only be reported after deployment.

The cost is that a prebuilt web directory (the release archive) cannot be
repointed without rebuilding. The archive's web files are served by
`artifree-server` on the API origin, where no default is needed, and release
archives are built without the variable. If a prebuilt static bundle is
distributed later (O03c), a runtime file can be added then; this decision does
not rule it out.

Precedence for the starting server: `?server=` in the link, then the address
kept in the tab's `sessionStorage` (chosen earlier in this tab), then the build
default, then the page's own origin. An empty Server URL field means the build
default when there is one, otherwise the page's own origin; the field is filled
with the default so the person sees where the token will go. The `?server=`
override keeps its rule that it suppresses restoring a stored token (0022); the
default does not, because the stored token is already scoped to the origin it
was typed for. With a build default the empty field therefore cannot mean the
page's own origin; type that origin in full, or build without the default. An
address kept for the tab that fails validation (for example one stored by an
earlier version) is reported and skipped.

### 2. Server addresses are validated

One function (`parseServerUrl`) validates every source: the link, the form, the
tab setting, and the build default. It accepts only absolute `http:` and
`https:` addresses, refuses credentials, a query string, and a fragment, and
returns the canonical form (lowercase host, default port dropped, no trailing
slash; a path prefix is kept because the client appends `/v1` to it). A
refused link or form entry is explained on the connect form, nothing is sent to
it, and the connection stays on the default. The messages never repeat the
address, since it may hold a secret. **A rejected `?server=` link still
suppresses restoring a stored token** (`fromQuery` stays true): the link tried
to name a server, so the page does not guess which stored token the person
meant, and a token is never offered to a server chosen by a link after the link
failed. The cost is that the person enters the token again for the stored or
default server; the failure is safe, not silent, because the notice says the
link was ignored. This is a behavior change for `?server=`:
relative values such as `/api`, which worked by accident, are now refused; they
were never documented.

### 3. Environment variables for the origin settings

`ARTIFREE_CONTENT_ORIGIN` and `ARTIFREE_FRAME_ANCESTORS` join the existing
`ARTIFREE_API_ORIGIN`, `ARTIFREE_ALLOWED_ORIGINS`, `ARTIFREE_ALLOWED_HOSTS`,
`ARTIFREE_TRUSTED_PROXIES`, `ARTIFREE_ALLOW_REMOTE_MCP`, `ARTIFREE_LAN`,
`ARTIFREE_LOOPBACK_LISTEN`, and `ARTIFREE_WEB_DIR`. The row names these two; the
others a container operator needs already existed, and no new variable was
added for listen addresses, data directory, or limits because the image fixes
the first two and the limits are outside this row.

- **Precedence** for single-value settings is flag, then variable, then the
  default (for the content origin, the `--lan` derived origin; for frame
  ancestors, the API-origin or listener-derived list). The list settings keep
  their earlier rule of using flag and variable together.
- **An empty variable is unset**, so a compose file or env file can declare an
  empty value to mean "not configured". A set value is validated; a value that
  is not valid UTF-8 is refused rather than ignored.
- **Same validation as the flags**, and the message names the source that was
  used (`ARTIFREE_CONTENT_ORIGIN must be a valid ...` or `--content-origin ...`).
  A flag in effect hides an invalid variable, including one that is not valid UTF-8: only the winning value is read.
- Equal API and preview origins are refused whichever way each was given, and
  the message names where each value came from (`ARTIFREE_CONTENT_ORIGIN must
differ from --api-origin ...`). `ARTIFREE_API_ORIGIN` errors name the variable.
- **Errors about serve settings never quote a value the operator wrote.** A
  rejected origin, source list, address, CIDR, or argument may carry a password
  (`https://user:secret@host`), and these messages go to stderr and container
  logs. This holds for every branch, not only the credentials check: a bad
  scheme, port, or host in a credentialed value fails earlier, so
  `normalize_origin` describes what is wrong without quoting the value or any
  part of it. The same rule covers `--frame-ancestors`, `--lan`,
  `--trusted-proxy`, and unrecognized `serve` arguments (`--content-origin=VALUE`
  names only `--content-origin`). The TypeScript client, whose address comes from `--server` or
  `ARTIFREE_SERVER`, drops credentials from the address in its network-error
  messages. `backup` and `restore` still name an
  unrecognized option, because their arguments are paths. The tests assert that a
  password in an origin that fails a scheme, port, host, or form check never
  reaches stderr.
- Parsing takes every `ARTIFREE_*` variable from one `ServeEnv` value read at
  start-up, not from the process environment at each use, so the unit tests
  give it exactly the variables they mean and pass on a machine that has others
  set.

Two validation gaps found above were closed for flag and variable alike,
because declaring them in a file makes mistakes easier to ship unseen:
`normalize_origin` now refuses credentials (without echoing them), and
`--frame-ancestors`/`ARTIFREE_FRAME_ANCESTORS` must be a non-empty,
space-separated list in visible ASCII without `;` or `,`. Source expressions
such as `'self'`, `'none'`, `https:`, and `http://host:*` remain valid. The
list is otherwise not parsed: a mistyped host is still accepted and simply
embeds nothing.

### Changing the CORS allow-list needs a restart

**Current behavior.** The CORS list, the allowed hosts, the trusted proxies, and
the preview policy (including `frame-ancestors`) are computed once when the
server starts and held in immutable shared state; the CORS middleware only
reads them. Changing any of them, by flag or variable, takes effect only on a
restart. A restart keeps the data (checked in CI for the archive and the
image) and ends open change streams (clients reconnect with resumption, 0006);
preview links and runtime sessions are kept in memory, so links issued before
the restart stop working and an open preview must be reopened.

**Decision: no reload mechanism.** We reviewed it and chose not to add one:

- The values are one security policy built into several derived forms (CORS
  list, host list, proxy matcher, CSP header). A reload has to swap them
  atomically with respect to in-flight requests and decide what a failed
  reload means; that needs its own tests and a trigger. `SIGHUP` does not exist
  on Windows, a supported target, so a file or endpoint would be needed, and an
  endpoint that changes security policy is a new authority to define (0023).
- In a container, environment changes already require recreating the container,
  so a reload would not help the main case this row is about.
- A restart is cheap and tested for a personal single-owner server.

The practical problem is a UI host that mints a new origin per deployment. The
guide says to serve the UI from a stable name; wildcard origins are not
supported and none were added.

**Related fix.** Reviewing the list showed that entries were compared as
typed, so `https://UI.example.app/` or `https://ui.example.app:443` never
matched the browser's `https://ui.example.app`, and the failure looked like a
CORS error in the browser. Entries are now normalized with the same function as
request origins, and an entry that is not an `http(s)` origin, or that carries a
path, query, or fragment (which normalizing would drop and so widen), stops the
server at start-up. `*` never worked and is now an error. This is the one change
to the CORS list itself, and it is **a compatibility change**: an existing
`--allowed-origin` or `ARTIFREE_ALLOWED_ORIGINS` value that was tolerated and
never matched now prevents start-up, so fix such an entry before upgrading.
Non-UTF-8 values of the older variables (`ARTIFREE_ALLOWED_ORIGINS`,
`ARTIFREE_ALLOWED_HOSTS`, ...) are still ignored silently as before; only the two
new variables refuse them.

## Verification

- Rust unit tests for precedence (flag over variable over default, including
  the `--lan` default and the API-origin default), normalization of the
  variable, refusal of invalid, credentialed, and equal origins, unsafe
  frame-ancestors lists from both sources, the source expressions in use today,
  and allowed-origin normalization and refusal.
- `scripts/tests/container-env.test.mjs` runs the built binary: the variables
  reach the preview policy and the reported origin, flags win, empty variables
  keep defaults, bad values exit 1 with the message, and an allowed origin typed
  unnormalized allows the browser's origin.
- `scripts/tests/web-server-defaults.test.mjs` tests the address validation and
  the starting-server precedence; `web-build-default.test.mjs` runs the real
  Vite build: the default is compiled in, absence leaves it out, and bad values
  fail the build without echoing a secret.
- The browser smoke suite (`scripts/smoke-static-default.mjs`, Chromium and
  Firefox) builds the page with a default, serves it from its own origin, and
  checks that the default is used and an empty field keeps it, a `?server=` link
  overrides it with no request to the default, unsafe links and typed addresses
  are refused before any request, and no script errors occur.
- `scripts/verify-container.mjs` starts the image with
  `ARTIFREE_CONTENT_ORIGIN` instead of the flag and requires three origin
  refusals. It runs only in GitHub CI; Docker was not available where this was
  written, so that part was **not executed locally**.
- Mutations of the guards (precedence, `;` refusal, credentials, empty-as-unset,
  build-time validation, link validation, scheme check, default-on-empty,
  allowed-origin normalization) each made a named test fail; see the pull
  request.

## Not changed, and gaps recorded

- No runtime configuration file, reload, wildcard origin, new variable beyond
  the two, TLS, or authentication. The Dockerfile and its entrypoint are
  unchanged.
- `--frame-ancestors` still replaces the default list instead of extending it.
- A default over plain HTTP from an HTTPS page is blocked by the browser; the
  page does not detect it beforehand.
- Not verified: a real static host, HTTPS between a hosted UI and API, a
  non-loopback origin in a browser, rootless Docker or Podman, and the image
  itself outside CI. Codex and Pi were not run.
