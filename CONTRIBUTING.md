# Contributing

Thanks for helping improve Codex Profiles.

## Before You Start

**For non-trivial changes (new features, significant refactors, breaking changes):**

Please open an [issue](https://github.com/midhunmonachan/codex-profiles/issues) or [discussion](https://github.com/midhunmonachan/codex-profiles/discussions) first to:
- Discuss your proposed changes
- Get feedback on your approach
- Confirm the feature/fix aligns with project goals
- Avoid spending time on work that might not be accepted

**For minor changes (bug fixes, typos, docs improvements):**

Feel free to open a PR directly.

## What We're Looking For

**Contributions we welcome:**
- Bug fixes with test coverage
- Documentation improvements
- Performance optimizations
- New features that align with the project's scope (profile management)
- Test coverage improvements
- CI/CD enhancements

**Out of scope:**
- Features that duplicate Codex CLI functionality
- Changes that compromise security (token handling, HTTPS enforcement)
- Breaking changes without strong justification
- Features that significantly increase complexity

## Setup

- Rust toolchain: `rustup show`
- Node (for npm packaging)
- `cargo-audit` for `make check` and `cargo-llvm-cov` for `make coverage`:

```bash
cargo install cargo-audit --locked
cargo install cargo-llvm-cov --locked
rustup component add llvm-tools-preview
```

## Codex compatibility

Before changing credential handling, refresh behavior, configuration lookup, or
usage parsing, compare the relevant code with current official OpenAI
documentation and the `openai/codex` implementation. Record the reviewed upstream
revision and any support limits in [compatibility.md](docs/compatibility.md).
Upstream internal types alone do not establish that an app-server method is
available; check its request registration and documented contract.

Account profiles here store credentials. Native Codex config profiles select
configuration layers. Keep that distinction in command behavior and user-facing
documentation, and preserve unsupported-store refusals.

Use synthetic credentials, temporary Codex homes, and loopback servers for tests.
Concurrency regressions should control request timing explicitly and bound waits.
Do not use a contributor's live accounts or auth files in automated checks.

## Checks

Run the same checks as the pre-commit hook:

```bash
make precommit
```

Other helpers:

```bash
make fmt
make clippy
make test
make coverage
```

`make coverage` is the standard check for the raw 100% line metric. Keep real I/O,
terminal, and concurrency tests; do not exclude production code. If a merged source
report appears covered while the raw gate fails, inspect per-function instances
because unit-test and CLI binaries can execute different paths. The requirement
measures executed lines, not 100% branch coverage.

## Dependency maintenance and merge checks

Dependabot checks Rust dependencies (including transitive dependencies) and GitHub
Actions weekly. Minor and patch version updates are grouped; major updates stay
separate. Security-update PRs are enabled separately in repository settings and
are not limited to the weekly version-update schedule. Updates are reviewed and
tested before merging; they are not automatically merged.

The npm platform packages are versioned together by the release process, so they
are not independently updated by Dependabot.

Every PR, including documentation-only changes, and every push to `main` runs the
Windows, macOS, Linux, security-audit, and coverage checks. The `main` ruleset requires these
GitHub Actions checks against an up-to-date branch, a pull request, and resolved
review conversations. It prevents force pushes and branch deletion, with no
admin or automation bypass configured. A second person's approval is not required for this
single-maintainer repository. The ruleset configuration is recorded in
`.github/main-ruleset.json`; editing that file alone does not update GitHub settings.

Releases publish attested checksums alongside their release assets, which is the
location used by the installer and verification guide. They do not push checksum
copies directly to protected `main`. Existing historical copies remain available.

## Pre-commit hook

Install the repo-managed hook wrapper (so updates are picked up automatically):

```bash
make hooks
```

This writes lightweight wrappers in your configured Git hooks directory
(respects `core.hooksPath`) that call the versioned hooks in `scripts/`
before each commit and push.

## Pull Request Guidelines

**Before submitting:**
- [ ] Run `make check` (or `make precommit`) - all checks must pass
- [ ] Add tests for new features or bug fixes
- [ ] Update documentation if behavior changes
- [ ] Keep commits focused and atomic
- [ ] Write clear commit messages

**PR description should include:**
- What problem does this solve?
- How does it solve it?
- Any breaking changes?
- Testing done (manual + automated)

**Review process:**
- Maintainers will review within a few days
- You may be asked to make changes
- Once approved, maintainers will merge

## Code Standards

- **Rust edition 2024** - follow existing patterns
- **100% line coverage** - enforced via `make coverage`, Linux CI, and the release verification job before artifacts are built or published. Production code must not be excluded to satisfy the gate. This measures executed lines, not every possible branch or proof of correctness.
- Python 3 is required for the Unix terminal integration tests, which exercise the real CLI prompts in isolated pseudo-terminals with synthetic credentials. The release and package smoke helpers require Python 3.11+ for their standard-library TOML parser.
- **No type suppression** - avoid `as any`, `#[allow]` without justification
- **Error handling** - proper `Result` types, no silent failures
- **Security-first** - especially around token/auth handling

## Release tag helper

Create a validated release tag that matches `Cargo.toml` and `package.json`:

```bash
make release-tag
```

To bump and tag in one step:

```bash
make release-tag ARGS="--bump patch"
```

`--bump` also syncs npm `optionalDependencies` package versions. `install.sh`
resolves the latest published release automatically, and you can still pin a
specific version with `CODEX_PROFILES_VERSION` or `--version`.

## Questions?

Not sure if your idea fits? Ask in [Discussions](https://github.com/midhunmonachan/codex-profiles/discussions) - we're happy to help!
