# Test and isolation practice

Status: contributor practice, 2026-11-05. The repository enforces only what
[quality.md](quality.md) lists: `npm run check`, `npm run build`, the browser
smoke checks, and CI. This page records how changes are tested and how heavy
commands are run, so that a contributor or agent can follow the same method
without private notes. Nothing here is checked by a script.

## Tests must be able to fail

- A test belongs in a change only if it fails when the behavior it guards is
  broken. Prove it: break the guard (a mutation), run the one test, and watch it
  fail. [quality.md](quality.md) requires every mutation to be restored; doing
  the edit in a throwaway copy makes that automatic. Listing each mutation and
  the test that caught it in the pull request is practice, not a requirement.
- If a mutation does not fail any test, say so in the pull request. A guard
  that nothing can observe is redundant or untested; do not claim coverage for
  it.
- Test behavior at the boundary that owns it ([quality.md](quality.md) lists
  them). Persistence, authorization, conflicts, retries, and process boundaries
  need behavioral tests, including the important rejection paths. A test that
  only repeats a field declaration or a placeholder is not added.

## Time, randomness, and the network are inputs

- Inject the clock, the random source, and `fetch`. Test waits with a fake
  clock; do not wait 45 real seconds. A fake clock should expose how many
  timers are pending, so a test can show that nothing is left running.
- Treat the transport as hostile but legal. For each new function that reads a
  stream, a timer, or a callback, ask and test: what if this promise never
  settles, a chunk is empty, a callback runs before the call that registered it
  returns, the body is already closed, the signal is already aborted or aborts
  at each step, or the headers arrive and no body follows?
- A timing-dependent failure is a finding. Do not widen a timeout, add a silent
  retry, or skip the test to make a run pass. Re-run once to see whether it
  repeats, then record the test name and the symptom in an issue or a roadmap
  row. A re-run that passes does not clear it: the required checks must pass
  on CI for the pull request, and a flake is not a reason to merge on the
  strength of a local re-run.

## Run heavy commands in isolation

- Heavy commands are `cargo` builds and tests, `npm run check`, `build`, and the
  test suites, browser smoke runs, servers, mutation experiments, and container
  builds.
- Run them in an environment that caps memory and CPU, has no network, has a
  private temporary directory and home, and works on a copy of the worktree. A
  mutation then never reaches the working tree, and only files declared up
  front (generated contracts, formatter output) are copied back.
- Without a network and with a private home, make available (read-only) what
  the checks need: the Rust toolchains, the cargo and npm packages, and the
  browser binaries. Install them first, outside the environment. A step that
  needs the network, such as downloads or advisory data, is confirmed on CI.
- State the cap you used, and report the peak memory of the heaviest run in the
  pull request, so growth shows. A process killed for memory under a cap
  smaller than the hosted CI runner's is a statement about the cap, not yet a
  defect: report the cap and the peak, and confirm on the CI runner, which is
  the baseline for the required checks. Do not widen a cap quietly to make a
  run pass; say what you changed.
- Do not build container images locally; rely on the pull request's CI and say
  so.
- The repository's commands work without any wrapper. The isolation tool is
  not part of the repository; use any equivalent (a container, a virtual
  machine, a cgroup scope) that gives the properties above.
- A separate worktree per change is useful when other work is present
  ([CONTRIBUTING](../.github/CONTRIBUTING.md)). Bound how many heavy runs go at
  once so that they cannot exhaust the machine.

## Leave nothing behind

After testing, remove what your run created: scratch copies, logs, and
processes you started, and the `node_modules` and `target` directories of a
disposable worktree before that worktree is removed. Do not delete caches that
other checkouts share. Check that `git status --short` shows only the intended
changes and that no test server or browser from your run is still alive.

## Review

Each PR receives one Codex review and one independent subagent review. Address
their findings with fixes or an evidenced explanation and run the relevant
checks; fixes and head changes do not require another review round. The review
procedure, including its deadline, merge requirements, and how a specified
feature's rules are amended, is in
[CONTRIBUTING](../.github/CONTRIBUTING.md).
