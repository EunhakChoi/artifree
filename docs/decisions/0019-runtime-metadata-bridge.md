# 0019: A bound runtime metadata read

Date: 2026-10-31. Status: proposed for this change. Builds on
[0003](0003-local-html-artifact-slice.md),
[0007](0007-live-web-workspace.md), and
[0015](0015-source-exports.md).

## Context and evidence

R04 needs one actual read from a separate preview frame, before R05 can
introduce declarations and grants and A08 can use a page binding. A captured
hosted HTML response contained a runtime injection, and a source contract
described artifact metadata. The capture is one observation, not proof that
every HTML document can safely be transformed. Host source also describes
initialization and view lifecycle; it does not establish the original
server's authorization policy. The private trace maps these requirements to
the examined evidence. The design below is an independent Artifree decision.

An opaque origin and `event.source` are insufficient on their own: a frame's
`WindowProxy` can survive navigation. A message port can also be deliberately
transferred. The account token must stay in the workspace even though the
artifact is allowed to navigate its own frame elsewhere.

## Decision

1. **Narrow opt-in.** An entry opts in with `<!-- artifree-runtime -->` as
   the first child of `head`, before author scripts. The supported prefix is
   an optional UTF-8 BOM and whitespace, a simple HTML doctype, then `html`
   and `head` opening tags, with quoted attribute values. Other entries use
   the existing byte-for-byte preview path, including reload. Source files,
   the existing `/p/` response, and downloads are never rewritten.
2. **First document initialization.** An opted-in runtime entry receives a
   generated bootstrap in place of that marker, before author code. Its
   `/r/` entry URL can serve that document only once; a later document in the
   same window cannot fetch its initialization again. Package resources are
   still read from the same pinned revision. This is a deliberate restricted
   HTML prefix, not a general HTML rewriting promise.
3. **Bound view.** The authenticated workspace creates a view with a random
   view id and increasing generation. The server issues an independent
   initialization secret and ticket bound to actor, artifact, pinned
   revision, current head, generation and expiry. Newer generations replace
   older ones atomically. Closed generations remain as bounded tombstones
   until expiry so a delayed old create cannot resurrect them. A restart
   invalidates all views; the workspace must prepare a new one.
4. **Message and RPC.** The frame sends its initialization with a new
   `MessagePort`; the workspace accepts exactly one port only from the
   current frame's window, with the expected initialization and generation,
   before expiry. The sole RPC reads current title and revision metadata.
   The workspace sends the authenticated request; the frame receives no
   account token. The server verifies the entire binding and current actor
   access before the read and rechecks the binding after it. A newer source
   head, deletion, a replaced generation, or expiry refuses the read.
   Closing or navigating the frame closes the port and aborts outstanding
   work; a result that finishes after close is not delivered.
5. **Exports.** An opted-in source export retains the source bytes and uses
   manifest `artifree.source-export.v2` with `runtime: "metadata"`. The
   bootstrap and its workspace are not bundled. Ordinary exports retain
   version 1 and `runtime: "none"`, as required by 0015.
6. **Generation.** `npm run generate:runtime` builds the server-embedded
   browser bootstrap from the TypeScript SDK with the pinned toolchain.
   `npm run check:runtime` rejects drift. The Rust server includes the
   generated text so a standalone Rust build needs no Node installation.

## Limits

This bridge grants only metadata about the frame's own artifact. It is not
the declaration, SDK pin, consent, grant or revocation model of R05; it does
not provide app data, source writes, models or agent handoff. Preview sandbox
and network limits remain those of 0003. As with any capability, an artifact
that intentionally obtains metadata can disclose it or delegate its own
message port; this design cannot prevent deliberate disclosure by that
artifact. Runtime views are process-local and bounded by the preview ticket
limit, not durable or shared across server instances.
