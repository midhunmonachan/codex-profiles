# Repository guidance

Read [CONTRIBUTING.md](CONTRIBUTING.md) and
[docs/compatibility.md](docs/compatibility.md) before changing behavior. Check
current issues and pull requests before starting nontrivial work, and agree on
feature scope through an issue or discussion. Keep changes focused and update
documentation and regression tests when behavior changes.

Keep current project status and outstanding decisions in
[docs/next-version.md](docs/next-version.md). Update that record when work ships
or scope is agreed; separate current status from dated historical research.

## Supported behavior

Codex Profiles manages file-backed account credentials. Preserve `CODEX_HOME`
resolution, existing saved profiles, and refusals for unsupported credential
stores and authentication modes. `--force` must not bypass those refusals.

Account profiles are distinct from native Codex configuration profiles.
Save/load currently handles authentication only.
[Optional config snapshots](docs/config-snapshots.md) are a design proposal;
unresolved design choices are not implemented behavior. Keep encrypted-export
design separate from snapshot storage and restoration.

Before changing credential handling, refresh, configuration lookup, or usage
parsing, check current official OpenAI documentation and relevant `openai/codex`
source. Internal types alone do not establish a supported app-server method.
Record reviewed revisions and limits in `docs/compatibility.md`. Advance
`.github/codex-compatibility.json` only after reviewing upstream changes.

## Validation and privacy

Use synthetic credentials, temporary Codex homes, and loopback servers in tests.
Never use live accounts or real auth files in automated checks. Keep real
credentials, private paths, personal contact details, and session history out of
fixtures, logs, commits, and reports. Use portable paths and clearly synthetic
examples in public documentation. Bound concurrency-test waits and control
request timing explicitly.

Use the pinned Rust toolchain and run `make precommit`. For automation changes,
also run `python3 -B scripts/test-automation.py` and relevant script tests.
Coverage has no minimum percentage; preserve meaningful real-I/O and concurrency
tests and do not exclude production code to improve the metric.

Track temporary resources created by the task and remove them after use.
Preserve existing work, unrelated sessions, and shared caches. Inspect the final
diff and report the checks actually run and any remaining limitations.

## Contribution and integration

Contributors, including additional collaborators and administrators, must use
pull requests to `main`. Only the owner account configured in the PR ruleset has
an exemption. Resolve review conversations on the PR path.

Five GitHub Actions checks apply to everyone, including the owner:
`tests-ubuntu-24.04`, `tests-macos-15`, `tests-windows-2025`, `security-audit`, and
`coverage`. Main also prohibits deletion and non-fast-forward updates.

For an authorized owner direct update, push the candidate to `codex/**`, wait
for all five push-event checks, then fast-forward `main` to that same commit.
Manually dispatched checks do not satisfy this route. The files
`.github/main-ruleset.json` and `.github/main-pr-ruleset.json` document policy;
editing them does not change GitHub enforcement.

Follow [CONTRIBUTING.md](CONTRIBUTING.md) and
[docs/verification.md](docs/verification.md) for release publication and
verification. Preserve historical changelog entries and published releases;
proposals and documentation maintenance do not imply a new release.
