# 0054: Keeping a runtime frame alive across a change of its view

> Browser engines: Firefox is suspended in CI until the planned feature set is
> complete. A Firefox acceptance below is met in Chromium and reported as pending
> re-verification ([quality](../quality.md#browser-engines)).

Date: 2026-11-11. Status: **proposed**; no code.
Roadmap: R10. Builds on [0019](0019-runtime-metadata-bridge.md),
[0020](0020-runtime-grants.md), [0032](0032-runtime-source-read-and-download.md),
[0033](0033-runtime-source-write.md), [0045](0045-uploaded-assets.md),
[0051](0051-roadmap-and-architecture-review.md),
[0052](0052-runtime-bridge-consolidation.md) and
[0053](0053-runtime-app-data.md). When its slices merge it amends the
"not implemented" notes of 0032 and 0033 that name R10, and rule 8 of 0033
where that rule says the workspace follows a save "in a new view" (rule 12
below); it changes no consent rule of 0020 or 0033.

## Context

A runtime frame is reloaded whenever its view changes. Three things change a
view today, and each one builds a new generation, a new document and an empty
JavaScript heap:

- **A permission change.** The user allows or revokes a capability. A page
  that asked for permission loses whatever the user had typed into it.
- **A save by the frame itself** (`source.write`, 0033). The head moves, the
  workspace follows the new revision in a new view, and the page that just
  saved starts again from nothing. 0033 recorded this as R10's subject.
- **A save by someone else** (an agent, the source editor, another tab), with
  the same effect on a page that had nothing to do with it.

A fourth limit has the same root: a view lives for its lease (15 minutes by
default) and nothing renews it, so a page left open stops working until the
user refreshes it, which also empties it.

The server already separates what matters here. A frame never holds the
view's secret after its first message: the workspace makes every server call
for it, addressed by view, generation and initialization secret. The frame's
document, its heap and its message port are one thing; the binding the
workspace uses on its behalf is another. Nothing in the model requires the
first to die when the second is replaced. What is missing is a way to replace
the binding of a live frame, and a rule for when that is allowed.

The policies below are independent Artifree design. No observation of another
product is claimed for them.

## Decision

### Opting in

1. A page opts in with one field of its manifest (0020, rule 1):
   `"lifecycle": "adopt"`. The only other value is `"restart"`, which is also
   the meaning of an absent field and of an entry without a manifest. Any
   other value makes the manifest malformed. The field is read from the
   revision a view pins, like the rest of the manifest, and the view the
   server returns names it.
2. A page that does not opt in behaves exactly as today: every change of its
   view reloads it. This is on purpose. Pages written so far read at load and
   rely on the reload to try again after a permission is given; keeping them
   alive would leave them showing a refusal that no longer holds.

### Adoption

3. **Adoption** replaces the binding of a live frame and leaves its document,
   heap and message port as they are. The workspace asks for it with the
   request that creates a view, plus the generation it replaces:
   `POST /v1/artifacts/{id}/revisions/{revision}/runtime-views` with
   `adopt: { generation }` beside `session_id`, `view_id` and the new, higher
   `generation`. No route is added.
4. The server adopts only when all of these hold; otherwise it answers as it
   does for a view it cannot issue, changes nothing, and the workspace
   restarts the frame (rule 11):
   - the named generation is this actor's live view with that `view_id`: not
     closed, not expired, its document and bootstrap served, on the same
     connection and the same artifact;
   - that view's manifest said `adopt`, and the manifest of the revision the
     new generation pins also says `adopt` with the same SDK pin;
   - the revision in the path is the one the old generation pins, or the one
     the old generation itself created with `source.write`. The server
     records that revision on the view when the write commits. A frame is
     never moved onto a revision it did not write.
5. An adopted generation is the same view record advanced in place: the same
   ticket, so the document's own address keeps serving its files; a new
   generation number, a new initialization secret and a new lease; the
   declared set of the pinned revision's manifest; the current head; and the
   grant the connection holds for `(artifact, pinned revision, head)` at that
   moment, exactly as a new view would get (0020, rule 3). It serves no
   document. The old generation's binding is gone from that instant, as with
   any replacement (0019): a call still addressed to it is `404`.
6. Adoption carries no permission. After a permission change the new
   generation holds what the user just set, because the grant's scope did not
   change. After a head change the scope is new and the generation holds
   nothing: the user allows again, as 0020 rule 4 and 0033 rule 8 require,
   and one consent to edit still covers one revision. What adoption keeps is
   the page, not its permissions.
7. The retry identity of a frame's write stays bound to its generation
   (0033, 0045, 0053). A write repeated after an adoption is a new write, and
   because every frame write is conditional on a base, it is refused as stale
   where the first one committed.

### When the workspace adopts

8. For a view whose manifest says `adopt`, the workspace does not reload the
   frame on its own. It adopts:
   - **when the user changes its permissions.** The frame's calls are paused
     (rule 9) before the change is sent, so no call meets the moment between
     the grant's new version and the new generation;
   - **when the frame's own save commits.** The new generation pins the
     revision the save made. The save's answer is given to the frame after
     the adoption;
   - **when another writer moves the head.** The new generation pins the
     revision the frame was loaded from, as a preview of an older revision
     does today, and holds no grant. The workspace says beside the preview
     that a newer revision exists and offers to load it;
   - **before the lease ends**, while the frame is shown, so a page left open
     keeps working for as long as its connection does.
9. **Pausing.** While the workspace adopts, it holds the calls the frame
   makes and sends none. It first waits for the calls already on their way,
   for at most 5 seconds; one that has not answered by then is abandoned and
   the frame is told `unavailable`. Held calls go out under the new
   generation once it exists. If the adoption fails, held calls are answered
   `expired` and the frame is restarted.
10. The workspace tells the frame of an adoption with one message that is not
    a reply (0053, rule 16, introduced the kind):
    `{ type: 'event', event: 'view.adopted', reason, leaseMs }`, where
    `reason` is `permissions`, `saved`, `head` or `lease`. The SDK moves its
    own deadline to `leaseMs` from now and calls the listeners registered
    with `window.artifree.view.onAdopt(listener)` with `{ reason }`.
    Registration sends nothing and returns a function that removes it, with
    the listener rules of 0053 rule 16. The frame is not told which
    permissions it holds; it learns that by calling, as today. An SDK that
    does not know the event ignores it and stops at its first deadline, which
    is today's behavior.
11. **Restart** is what happens today, and it remains the answer whenever
    adoption is not possible: a new view, a new document, no heap. The
    workspace restarts a frame when the user refreshes the preview or chooses
    another revision, when the page navigated itself, when the connection was
    replaced, when the manifest no longer says `adopt`, and when an adoption
    is refused or fails.
12. A view that was adopted runs the document it was loaded with. After its
    own save the stored source may differ from it (the page may have saved
    its own entry file). The workspace therefore shows, for an adopted view
    whose pinned revision is not the one its document was loaded from, that
    the preview kept running across a save and that refreshing loads the
    saved revision. This replaces 0033 rule 8's "in a new view" for a page
    that opted in; the rest of that rule stands.

### Everything else that follows the view

13. What the workspace keeps for a view moves with the adoption or ends with
    it, never both. The change hint (0053, rule 17) is re-established for the
    new generation, and only if it holds `data.read`. A download prompt that
    is open is withdrawn. The write budget and the frame budget (0053, rule 23) continue; an adoption does not refill them. An answer to a call of
    the old generation that arrives after the adoption is dropped.
14. The host, the client and the SDK change as 0052 lays out: the client's
    view creation takes the optional adoption, the host's rpc gains the pause
    and the replacement of its view, and the SDK gains the event and
    `window.artifree.view`. `runtime-bridge-layers.test.mjs` pairs the new
    event.

### What does not change

15. Consent, grant scope and versions (0020), the check order of every frame
    route, the retry keys, the fences on the head inside write transactions,
    the lease length, the connection's lifetime, the sandbox, the CSP and
    token handling are unchanged. A frame still holds no secret. A page
    without the manifest field sees no difference.

### Acceptance

16. Against a real server: an adoption for each of the four causes; each
    refusal of rule 4 (a closed, expired, unserved or foreign generation,
    another connection, another artifact, a manifest without `adopt`, a
    changed SDK pin, a revision the view neither pins nor wrote), with the
    old view left as it was; the old generation refused after an adoption; a
    late create and a late close for a generation below the adopted one; an
    adopted generation holding exactly the grant of its scope, and none after
    a head change; a second save from an adopted view after the user allows
    editing again; a write repeated across an adoption refused as stale; the
    document's address still serving files after an adoption and no document
    served twice. Each guard is proved by mutation.
17. Unit checks of the host's pause (held calls, the 5 second bound, a failed
    adoption), of the client and of the SDK's deadline and listeners; and a
    Chromium smoke in which a page that opted in keeps a value typed into it
    across a permission change, across its own save followed by a second
    save, and across a save by another writer, while a page that did not opt
    in is reloaded as before.
18. `npm run check`, `npm run build`, the bundle sizes of 0052 rule 12, and
    no growth of the web entry bundle from on-demand code.

### Slices

| Slice | Scope                                                                 | Rules             |
| ----- | --------------------------------------------------------------------- | ----------------- |
| R10a  | The manifest field, the contract, and adoption on the server          | 1, 3 to 7, 15, 16 |
| R10b  | The client's request, the host's pause and replacement, the SDK event | 9, 10, 13, 14, 17 |
| R10c  | The workspace's adoption for each cause, the notices, the smoke       | 2, 8, 11, 12, 17  |

## Alternatives considered

- **Carrying permissions to the new generation after a save.** It would
  spare the user the second consent. 0033 rejected re-scoping a grant to the
  new head because one click would let a page rewrite the artifact again and
  again unobserved; carrying everything except `source.write` avoids that,
  but still gives capabilities to a view at a head the user has not seen.
  Not chosen now. It can be added later without changing adoption itself,
  since it only changes which grant rule 5 finds.
- **Reloading with a snapshot.** The old document would hand a bounded value
  to the workspace, which would give it to the new document. This keeps
  state across a reload, also for a new revision's code, but it moves a value
  from one revision's heap into another's, which is the boundary the roadmap
  row asks to keep, and it asks every page to write its state out and read
  it back. A page that wants state to survive a reload can already keep it
  in application data (0053).
- **Adopting every frame.** Simpler for the workspace, and it would break
  pages that rely on the reload (rule 2).
- **Opting in from the SDK at run time.** The server could not know, when it
  is asked to adopt, whether the page had agreed, and the workspace would
  have to trust a message from the frame for a decision about its binding.
  The manifest is in the revision and both can read it.
- **A lease renewal route of its own.** Renewal is an adoption with no other
  change; one mechanism has one set of checks.
- **Letting the frame defer a replacement.** The frame would be asked before
  a restart and could delay it. With adoption a page that opted in is not
  restarted by another writer's save at all (rule 8), which is what a delay
  was for, and the user's own refresh should not be something a page can
  hold up.

## Consequences

- A page that opts in keeps what the user was doing through a permission
  change, its own save, another writer's save and the end of a lease. It must
  handle a refused call by trying again later or on `onAdopt`, because the
  reload no longer does that for it.
- After its own save such a page runs a document that may be older than the
  stored source, until the user refreshes. The workspace says so.
- **Not implemented by this record:** carrying permissions across a head
  change; a snapshot handed to a new document; adoption for a page that did
  not opt in; telling a frame which permissions it holds; a frame asking for
  its own restart; adoption across a replaced connection or a server
  restart, which end every view as today.
- A permission change made through another preview of the same revision on
  the same connection reaches this frame after the fact: its calls are
  refused until the workspace has adopted it, since the pause of rule 8 can
  only precede a change the same preview makes.
