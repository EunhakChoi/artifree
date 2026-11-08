## Change

<!-- What concrete problem does this solve, and what happens afterwards?
Link the issue/roadmap item where relevant. Describe the final implementation. -->

## Design and compatibility

<!-- Explain the choices a reviewer needs: boundaries, API/data compatibility,
dependencies, migrations, UI variants/tokens, or justified exceptions. Link ADRs.
For reconstruction, label observations, source-only evidence, inference, and
Artifree decisions. Do not include credentials, local data, or private evidence
and its paths. -->

## Verification

<!-- Report what actually ran on this change. Include the regression's original
failure and important rejection/retry/recovery behavior. Separate synthetic or
mocked results from real systems. For guard mutations, name the intended failing
assertion and restored baseline. Explain missing/inapplicable checks below. -->

| Command or scenario                        | Outcome / evidence |
| ------------------------------------------ | ------------------ |
| `npm run check`                            |                    |
| `npm run build`                            |                    |
| Chromium smoke, for UI/HTML changes        |                    |
| Firefox pending, if an acceptance names it |                    |

## Limitations

<!-- Material risks, untested integrations, unrun checks, and bounded follow-up.
Use "None" if there are none. Keep credentials and private data out of evidence. -->

## Review record

<!-- One Codex review (automatic on creation) and one independent subagent review.
The subagent uses a fresh context to check arbitrary interpretations of abstract
or undecided requirements and applicable quality rules; for docs, check policy
consistency. Address findings without another review round. Record subsequent
fixes and their checks. The Codex deadline is 15 minutes including queue time;
record errors/timeouts honestly, not as completed reviews. Evidence may be linked
from a PR comment without another documentation commit. -->

| Reviewer             | Trigger / request link   | Reviewed full head SHA / local diff | Requested / deadline / completed (UTC) | Result / findings / fixes / evidence |
| -------------------- | ------------------------ | ----------------------------------- | -------------------------------------- | ------------------------------------ |
| Codex                | Automatic on PR creation |                                     |                                        | Pending                              |
| Independent subagent |                          |                                     |                                        | Pending                              |

## Author checks

- [ ] The final scope, behavior, and relevant decisions are documented.
- [ ] Required checks passed; actual results and omissions are reported.
- [ ] Important guards have behavioral tests with verified failure detection.
- [ ] Affected generated outputs, lockfiles, and roadmap entries are current.
- [ ] The change excludes credentials, local data, private evidence, generated verification outputs, caches, and unrelated changes.
- [ ] One Codex review and one independent subagent review are completed, and their findings are addressed.

<!-- Review policy: CONTRIBUTING.md. Do not put bot mentions in this description.
Do not request another review solely because findings were fixed or HEAD changed.
Require both completed reviews, addressed findings, successful current-HEAD CI,
resolved conversations, and no conflicts before merging. Verify the actual main
merge commit's CI afterwards. Do not record pending work as complete. -->
