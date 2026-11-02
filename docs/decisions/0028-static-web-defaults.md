# 0028: Static web defaults and container settings

Date: 2026-11-02. Status: accepted. Builds on
[0022](0022-production-web-and-api-serving.md),
[0024](0024-https-proxy-and-remote-access.md), and
[0026](0026-container-image.md). Roadmap row: O03d. Independent Artifree design
for Artifree's own deployment settings; no original-product behavior is used.
This is a rewrite of a first attempt that did not pass review (see "Rewrite
after review").

## Context

O03b showed that a static copy of the web workspace and a container can be run
today, but only with arguments: every new tab needs `?server=` or the Server
URL field, and `--content-origin` and `--frame-ancestors` can only be given as
flags, so a container or a static site cannot declare them in its environment
file. The row also asks whether the CORS allow-list needing a restart is a
problem. Reading the code for this row showed three weaknesses that the new
settings would have inherited, so they are fixed here: the origin parser
silently dropped credentials and read an unusable port as "no port",
`--frame-ancestors` was not checked at all, and several start-up errors quoted
the operator's value, passwords included.

## Decision

### Static web: a build-time default server

1. **`VITE_ARTIFREE_SERVER=<address> npm run build`** sets the server a static
   build uses when nothing else names one. A runtime configuration file was
   considered and not chosen: the common case has no file, so every page load
   would make a request that ends in a 404 and a console error, and the smoke
   suite requires zero console errors. The cost of the build-time choice is one
   build per target; a runtime file stays a candidate if that becomes a burden.
2. **One function, `parseServerUrl`** (`apps/web/src/artifacts/server-url.ts`),
   decides every source: the `?server=` link, the connect form, the address
   kept for the tab, and the build default. An address must be absolute
   `http` or `https`, with no user name or password, no query and no fragment
   (an empty `?` or `#` counts), and no backslash. It is returned as
   `origin + path` without a trailing slash. The Vite config uses the same
   function: a build default it refuses **fails the build**, and the message
   names the variable and the rule, not the value.
3. **Precedence: link, then the address kept for the tab, then the build
   default, then the page's own origin.** A source that fails the check is
   skipped, never requested, and explained with a sentence that does not repeat
   it ("The server address in the link was ignored because it contains a user
   name or password").
4. **The build default is not kept in `sessionStorage`.** `serverToKeep`
   returns nothing for the page's origin and for the build default, so a tab
   that connected to the default stores no server address and a later build
   with another default is followed. An address the person chose (a link or the
   field) is kept for the tab, as before (0022).
5. **An empty Server URL field means the default** (the build default, else the
   page's own origin). The field is pre-filled with the starting address and
   says what empty means. The consequence is that with a build default present,
   the page's own origin can only be chosen by typing its address. An address
   typed into the field is checked by `parseServerUrl` before anything is sent.
6. **Tokens and notices.** The connect form is the only place that shows why an
   address was ignored. So a token is **not restored automatically** when the
   link named a server, or when any address was ignored during start-up;
   otherwise the person would be connected to a fallback server without ever
   seeing the notice. The tab-stored token for a valid, un-noticed starting
   server is restored as before. Notices are cleared as soon as the person
   connects or disconnects, so the form does not show them a second time.

### Environment variables for `serve`

7. **`ARTIFREE_CONTENT_ORIGIN` and `ARTIFREE_FRAME_ANCESTORS`** are the
   variables of `--content-origin` and `--frame-ancestors`. Precedence is flag,
   then variable, then the existing default. An empty variable is unset (an
   empty flag is an error). A variable that is not valid UTF-8 is an error
   **only when no flag overrides it**.
8. **One injectable environment.** `ServeEnv` (`serve_env.rs`) copies the ten
   variables `serve` understands from the process once, in `main`. The option
   parser only receives the struct, so unit tests build their own and do not
   depend on the process environment; the bin tests pass with hostile
   `ARTIFREE_*` values set. The older variables keep their meaning: a flag wins
   over `ARTIFREE_WEB_DIR`, `ARTIFREE_LAN`, `ARTIFREE_LOOPBACK_LISTEN` and
   `ARTIFREE_API_ORIGIN`; the list variables (`ARTIFREE_TRUSTED_PROXIES`,
   `ARTIFREE_ALLOWED_HOSTS`, `ARTIFREE_ALLOWED_ORIGINS`) are added to the
   flag's entries; `ARTIFREE_ALLOW_REMOTE_MCP` enables on `true` or `1`. For
   all of them an empty or non-UTF-8 value is **silently treated as unset**
   (unchanged, and different from the two new variables).

### One strict origin parser

9. **`artifree_server::origin::normalize_origin`** is used for
   `--content-origin`, `--api-origin`, `--allowed-origin` and their variables,
   and by the CORS middleware for the request `Origin` header. It is written
   out, not built on a URI library, because `http::Uri::port_u16()` reports "no
   port" for `:65536` or `:abc`, which turned `https://ui.example:65536` into
   the default-port origin `https://ui.example`. Rules, checked before anything
   is canonicalized: scheme `http` or `https` (any case); then `://`; the host
   is a registered name (letters, digits, `-` and `_`, non-empty labels, no
   leading or trailing `-`, no trailing dot), a dotted-quad IPv4 address (digits
   and dots must form a valid address; `1.2.3` and `01.2.3.4` are refused), or
   a bracketed IPv6 address; if a `:` follows the host the rest must be digits
   only and from 1 to 65535 (`:`, `:0`, `:65536`, `:abc`, `:-1`, `:08080x` are
   errors; leading zeros are accepted, so `:08080` is 8080); no `@`, and no
   path, query or fragment except **one trailing `/`**, which the earlier
   parser also accepted. Surrounding whitespace is trimmed; any other
   whitespace, control or non-ASCII character is an error (no internationalized
   names; use the punycode form). The result is lowercase, drops the scheme's
   default port, and uses the compressed IPv6 form.
10. **`AppState::new` repeats the checks** on the content origin, the API
    origin and every allowed origin, canonicalizing and de-duplicating the
    allow-list, so a library caller cannot put an unvalidated entry in it.

### Errors never quote an operator's value

11. An error on the `serve` path carries a **static reason and the name of the
    flag or variable in effect**, never the value or any part of it, and never
    the lenient parser's own message. This covers origins, `--lan`,
    `--loopback-listen`, `--api-listen`, `--content-listen`, CIDR entries
    (`IpNetwork`'s errors are fixed strings), frame ancestors, numeric options,
    and unrecognized arguments. An argument that is not a known option is named
    only if the text before any `=` matches `--[a-z0-9-]+`; otherwise the
    message says only that an argument is not a serve option. A non-UTF-8
    argument is reported without its text. `--lan 0.0.0.0` no longer shows the
    address. The TypeScript client's network-error messages show at most
    `origin + pathname` for an http(s) address and "the configured server" for
    anything else, and the error's `cause` (which a runtime may fill with the
    URL it was given, credentials included) is attached only when the address
    is plain: http(s) without credentials, query or fragment. This holds for
    the send path, the file-body read path, JSON body reads and the change
    stream.
12. **What is still shown, deliberately:** the bound socket address in "cannot
    listen on ..." (it was parsed as an IP address and port, so it cannot hold
    a credential); file-system paths such as `--data-dir` and `--web-dir` in
    their own errors (the operator's own paths, not secrets); the names of the
    flags and variables. `--allowed-host` entries are not validated and cannot
    produce an error. **The "must differ" error cannot say which of the two
    sources set a value**, so it names both flags and both variables. The
    library errors of `AppState::new` name the setting ("an allowed CORS
    origin"), not a flag, because the library has no flags; through the command
    line they are only reachable after the same checks passed.

### Frame ancestors

13. `--frame-ancestors` and `ARTIFREE_FRAME_ANCESTORS` take a space-separated
    source list. It is **refused** when empty, when `'none'` is combined with
    other sources (browsers drop such a directive, leaving embedding
    unrestricted), or when any source contains `;` or `,` (a `;` would end the directive and start another; a comma is the
    separator of other headers and always a mistake here) or a character that
    is not visible ASCII. Sources such as `*`, `'self'`, `https:` and
    `https://ui.example:*` are kept; they are not checked beyond that, so
    CSP source syntax is still the operator's responsibility. The check lives
    in `preview::normalize_frame_ancestors`, which `content_policy` also uses.
    Runs of spaces become one space.

### CORS allow-list and restart

14. The allow-lists (`--allowed-origin`, `--allowed-host`, the API origin), the
    trusted proxies and the frame ancestors are read once at start-up and held
    in immutable shared state; changing any of them needs a restart. **No reload
    mechanism is added.** A reload would need a trigger (signal or endpoint), an
    atomic swap that every middleware and the preview policy see together, and
    tests for partial failure, for a security setting whose mistakes fail open.
    A restart is cheap and safe for data: everything durable is in SQLite and
    shutdown is graceful (0026). It does drop what lives in memory: preview
    links, runtime sessions and agent handoffs (0019 to 0021). The guides now say
    so, and that an invalid replacement stops the restarted server with an
    error rather than keeping the old list.

### Container verification

15. The CI `container` job now starts the image with
    `ARTIFREE_CONTENT_ORIGIN`, `ARTIFREE_ALLOWED_HOSTS` and
    `ARTIFREE_FRAME_ANCESTORS` instead of flags, and checks that the preview's
    CSP carries the list from the variable. Its refusals are: equal API and
    preview origins set by variables, an invalid `ARTIFREE_CONTENT_ORIGIN` whose
    log must not contain the password in it, and an open data directory. The
    Dockerfile is unchanged. This ADR supersedes decision 5 of 0026 ("we did not
    add an environment variable"): its objection was a new configuration surface
    for one deployment form, and the variables are the server's, not the
    container's.

## Compatibility changes

- `--allowed-origin` / `ARTIFREE_ALLOWED_ORIGINS` entries that are not valid
  http(s) origins **stop start-up**. `*`, bare hosts (`ui.example`), entries
  with a path, and an out-of-range or non-numeric port were accepted before and
  could never match a browser's `Origin`; now they are errors. Entries are
  canonicalized, so `HTTPS://UI.example:443/` now matches `https://ui.example`.
- `--content-origin` and `--api-origin` refuse credentials (they were dropped
  silently), unusable ports (read as the default port), and other lenient
  shapes.
- The CORS middleware answers 403 to a request `Origin` with an unusable port
  instead of reading it as the default port.
- `--frame-ancestors` refuses an empty list, `;` and `,`.
- The web workspace no longer accepts a relative Server URL (such as `/api`),
  an address with credentials, a query or a fragment, or one that is not http
  or https, in the field, in `?server=` or in a value kept by an older tab. An
  empty field still means the default (the page's own origin unless a build
  default exists).
- Some messages changed: `--lan` no longer shows the address, an unrecognized
  option shows only its name, CIDR errors are fixed strings.

## Rewrite after review

A first attempt (PR 59, closed) passed no review round. Round 1 found
credentials echoed by origin errors and tests that depended on the process
environment; round 2, a skipped-address notice lost when a token for a fallback
server was restored and an unrecognized-option error that repeated a whole
argument; round 3, that `--allowed-origin https://ui.example:65536` was
canonicalized to the default-port origin and added to the CORS allow-list. Each
was a gap in ad-hoc validation added on top of a lenient parser. This version
starts from one explicit parser and one rule for messages, and keeps the change
to what the row asks: no runtime configuration file, no reload, no new command.
The decisions above that exist because of the review are 3 and 6 (skipped
addresses and tokens), 4 (the default is not stored), 8 (one injectable
environment), 9 (the parser), 11 (the message rule), and 13 (frame ancestors).

## Not done, and not verified

- No runtime configuration file for the static build, no hot reload of any
  server setting, no per-source CSP validation, no validation of
  `--allowed-host`.
- The static build with a default was exercised in Chromium and Firefox against
  two real server processes over plain HTTP on loopback, from a static file
  server. No hosting service, HTTPS, or second machine was used.
- The container job runs only in GitHub CI; Docker was not available where this
  was written, so the environment-variable run of the image is unverified until
  that job passes. The command-line behavior is verified through the binary
  (`scripts/tests/container-env.test.mjs`).
