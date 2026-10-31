## Problem and result

<!-- Describe the concrete trigger and resulting behavior. Link the issue or
roadmap item if applicable. Write for a reviewer who has not seen the session. -->

## Design

<!-- Explain material choices, compatibility, and any changed boundary, contract,
dependency, migration, UI token/variant, or narrow exception. Link relevant ADRs.
For reconstruction work, separate observations, source-only evidence, inference,
and Artifree decisions; keep private evidence paths and vendor material out. -->

## Validation

<!-- List commands actually run on the proposed code and exact outcomes. Include
the reproduced regression and important refusal/retry/recovery cases. Explain
unrun or inapplicable checks. Distinguish injected responses from real services.
For new guards, record the intended failure under mutation and restored baseline.
Never include tokens, account data, or private captures in the evidence. -->

| Check or scenario                             | Result and evidence |
| --------------------------------------------- | ------------------- |
| `npm run check`                               |                     |
| `npm run build`                               |                     |
| Chromium / Firefox smoke (UI or HTML changes) |                     |

## Limits and follow-up

<!-- State material limits, unverified integrations, and follow-up with a reason.
Write "None" when there are none. Do not present planned behavior as implemented. -->

## Checklist

- [ ] Scope, design, and public behavior match the final diff and relevant ADRs.
- [ ] Required local checks passed; results and unrun checks are reported above.
- [ ] Important guards have behavioral coverage and evidence that they can fail.
- [ ] Generated outputs, lockfiles, documentation, and roadmap status are current where affected.
- [ ] No proprietary implementation, secrets, account data, or private evidence paths are published.

<!-- Maintainers: follow CONTRIBUTING.md. The first Codex review is automatic;
subsequent rounds use @codex review. Each round has a 15-minute limit and each PR
has at most three rounds, including errors/timeouts. After three unsuccessful
rounds, close and independently rewrite from latest main on a new branch.
Merge only with a clean review and required CI on the current HEAD. -->
