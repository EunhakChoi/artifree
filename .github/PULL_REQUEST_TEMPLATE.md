## Change

<!-- What concrete problem does this solve, and what happens afterwards?
Link the issue/roadmap item where relevant. Describe the final implementation. -->

## Design and compatibility

<!-- Explain the choices a reviewer needs: boundaries, API/data compatibility,
dependencies, migrations, UI variants/tokens, or justified exceptions. Link ADRs.
For reconstruction, label observations, source-only evidence, inference, and
Artifree decisions. Do not include private evidence paths or vendor code. -->

## Verification

<!-- Report what actually ran on this change. Include the regression's original
failure and important rejection/retry/recovery behavior. Separate synthetic or
mocked results from real systems. For guard mutations, name the intended failing
assertion and restored baseline. Explain missing/inapplicable checks below. -->

| Command or scenario                             | Outcome / evidence |
| ----------------------------------------------- | ------------------ |
| `npm run check`                                 |                    |
| `npm run build`                                 |                    |
| Chromium and Firefox smoke, for UI/HTML changes |                    |

## Limitations

<!-- Material risks, untested integrations, unrun checks, and bounded follow-up.
Use "None" if there are none. Keep credentials and private data out of evidence. -->

## Author checks

- [ ] The final scope, behavior, and relevant decisions are documented.
- [ ] Required checks passed; actual results and omissions are reported.
- [ ] Important guards have behavioral tests with verified failure detection.
- [ ] Affected generated outputs, lockfiles, and roadmap entries are current.
- [ ] Code and evidence contain no proprietary implementation or private data.

<!-- Review policy: CONTRIBUTING.md. First Codex review is automatic; later
rounds use @codex review. Deadline: 15 minutes per round. Maximum: three rounds
including service failures/timeouts. After three unsuccessful rounds, close and
independently rewrite from latest main on a new branch. Require current-HEAD
finding-free review, required CI, and resolved conversations before merging. -->
