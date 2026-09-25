# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2026-09-25

### Added

- `status --all --compact` groups short usage and reset-time summaries by account
  and usage bucket, including accounts with unavailable usage or errors.
- `doctor` reports local Codex configuration compatibility without displaying
  configuration secrets or modifying `config.toml`.
- Status JSON preserves upstream `primary` and `secondary` windows, each with
  its actual `window_seconds`, remaining percentage, and reset timestamp.
- Real-terminal regression tests cover account switching, cancellation, deletion,
  and files changing while a confirmation prompt is open.

### Changed

- Run tests and report Rust line coverage locally, in CI, and before release
  without requiring a minimum coverage percentage.
- Require PRs from contributors while allowing only the owner's account to push
  directly to `main`; required checks and branch protections apply to everyone.
- Update Rust dependencies and pinned GitHub Actions across the release and CI
  workflows.

### Fixed

- Preserve the active account before switching and refuse stale refresh writes
  when authentication changes during a request.
- Refresh selected active accounts through the active auth file and preserve
  their rotated tokens in the saved account profile.
- Preserve the stored `account_id` during refresh, matching Codex's pinned
  account identity so a new `id_token` account claim cannot silently select a
  different saved account.
- Prefer exact token snapshots when syncing duplicate account aliases; allow a
  sole alias to absorb token rotation, fail closed when multiple aliases have
  no exact active snapshot, and keep status from overwriting an ambiguous alias.
- Reject case-insensitive reserved and duplicate import IDs before writing profiles,
  including aliases that collide on Windows filesystems.
- Apply Codex auth-mode precedence to imports, rejecting unsupported credential
  modes before writing imported profiles or index metadata.
- Report weekly-only and custom-duration usage windows accurately. The existing
  `five_hour` and `weekly` JSON aliases now match only those exact durations.
- Report empty usage responses as unavailable and stop retrying when the service
  requests a delay beyond the interactive retry budget.
- Remove temporary credential files after failed atomic writes, preserve the
  existing destination on replacement failure, and set private Unix permissions
  before publishing copied credentials.
- Refuse mutations when the profile index is malformed, preserving metadata for
  explicit recovery with `doctor --fix`.
- Preserve executable permissions in native release archives and verify artifact
  hashes, binary contents, npm package metadata, Homebrew checksums, and permissions
  before publishing.
- `--version` and `-V` now print to stdout and exit successfully.
- Keep Cargo.lock in sync when the release helper bumps the version.
- Test built binaries and npm packages before publishing, including version/help
  exit codes and profile switching.

### Documentation

- Explain account profiles versus native Codex configuration profiles, record the
  September 24 upstream source review, and document usage JSON compatibility.

## [0.3.2] - 2026-09-15

### Added

- `load --with-status` shows usage and reset times after switching profiles.
  Supports JSON output, and the profile still switches if the status check fails.
  Thanks to @Chaitanya-Keyal for the contribution.

### Changed

- Updated Rust dependencies, including the `serde_with` security fix.
- Updated release and test tools, added weekly dependency checks, and required
  passing checks before merging.
- Added a CI check for the project's 90% minimum line coverage.

## [0.3.1] - 2026-09-14

### Fixed

- Updated `crossbeam-epoch`, `rustls`, `rustls-webpki`, and transitive `anyhow` dependencies
  to address the September 2026 security-audit findings.
- Honor `CODEX_HOME` directly for credentials, configuration, and profile storage.
- Match Codex authentication-mode precedence, reject unsupported externally managed
  authentication, and align OAuth refresh request fields with current upstream source.
- Read the documented `cli_auth_credentials_store` setting using TOML scope rules;
  keep the earlier `_mode` spelling as a fallback and reject invalid configuration.
- Refuse `load` into unsupported credential stores before modifying credentials or
  the profile index, including with `--force`.
- Read usage endpoint configuration only from the TOML root table.
- Fix Windows test portability and compile warnings, including complete mock HTTP request reads.

### Documentation

- Explain CLI/IDE/desktop compatibility limits, storage modes, custom home paths,
  and network use during usage checks and credential refresh.

## [0.3.0]

### Added

- `doctor` command with `--fix` and `--json` for scriptable diagnostics and safe storage repairs
- profile label management commands: `label set`, `label clear`, and `label rename`
- profile export and import commands for backup and transfer bundles
- exact `--id` selectors for `load`, `delete`, and `status`, plus saved-profile `status` selectors with `--label` and `--id`
- `load --force`, `list --json`, `list --show-id`, and machine-readable `--json` output for mutating commands, `status`, and `doctor`

### Changed

- `--json` is now a global CLI option, and mutating commands emit a consistent success response shape
- `status` now refreshes usage auth on `401` responses, syncs refreshed credentials back to the saved active profile when possible, and formats auth/usage HTTP errors in a Codex-style layout
- `status --all --json` now returns a single `profiles` array that includes API-key and errored profiles, and human-readable output now uses "active profile" wording
- top-level `--help` output now uses shorter examples, highlights `--json` support more clearly, and label subcommands now spell out the required `--label`/`--id` selector more clearly
- the manual installer now resolves the latest published GitHub release automatically when no version is pinned, while still honoring `CODEX_PROFILES_VERSION` and `--version`
- Cargo installs now require Rust 1.94 or newer
- package and README metadata now describe the tool in terms of switching between multiple Codex accounts
- status usage lookups now only allow official ChatGPT hosts or loopback addresses for `chatgpt_base_url`, and non-file Codex auth store modes are rejected

### Fixed

- usage fetching now retries transient transport, `5xx`, and rate-limit failures more robustly and supports grouped multi-bucket usage limits
- installer and release downloads now verify checksums from the tagged GitHub release by default, with a guarded insecure bypass for environments that explicitly set `CODEX_PROFILES_ALLOW_INSECURE_INSTALL=1`
- npm and Bun update-source detection is more reliable
- regular remote error output now uses aligned multiline blocks while JSON output preserves raw backend detail
- errors always exit non-zero and write to stderr only; stdout is never polluted with partial JSON on error

### Removed

- `status --all --show-errors` flag and hidden-profile summary/count modes

### Internal

- `anyhow` dependency was removed; `updates.rs` now uses `Result<T, String>` consistently
- auth refresh now refuses to rewrite drifted on-disk auth state
- release packaging now emits a release manifest, verifies generated artifacts more strictly, and publishes npm packages with provenance plus GitHub attestations
- release and CI workflows were hardened, pinned more explicitly, and now include a scheduled security-audit workflow
- integration and regression coverage expanded for JSON output, release artifacts, status/profile edge cases, and profile storage repair paths

### Documentation

- README now documents export/import, label workflows, doctor repair behavior, and release verification more thoroughly
- added a release verification guide covering `SHA256SUMS`, `release-manifest.json`, GitHub attestations, npm provenance, and `.crate` verification

## [0.2.0] - 2026-02-21

### Added

- `status --all --show-errors` to include errored profiles in the output
- Hidden-profile summaries in `status --all` (API profiles and errored profiles)

### Fixed

- Profile deduplication now uses a composite identity key (`principal_id` + `workspace_or_org_id` + `plan_type`) to prevent false matches
- Profile lifecycle operations now preserve update cache and handle save/load/delete edge cases more safely
- Installer trap variable scoping bug that could break cleanup on some shells
- Release bump flow now syncs npm optional package versions and installer default `VERSION` correctly
- CI cache config updated to remove deprecated `rust-cache` parameters and invalid `save-if` usage

### Changed

- Simplified profile state machine and status rendering logic
- Profile ordering changed from last-used timestamp sorting to deterministic profile-id ordering
- `list` and `status` no longer perform implicit current-profile sync side effects
- "Unknown last-used" badge is now hidden instead of shown as a placeholder

### Removed

- Last-used timestamp tracking in `profiles.json` profile metadata
- Legacy `cx` shorthand script (use `codex-profiles` directly)

### Internal

- Installer and release scripts hardened for edge-case reliability
- CI: removed unused `sccache` from workflow jobs
- Added repository checksum file for previous release (`checksums/v0.1.0.txt`)

### Documentation

- README: reorganized badges, added a tests badge, and fixed license badge color
- README: added Cargo installation instructions
- CONTRIBUTING: documented release-tag bump behavior for npm optional packages and installer defaults

## [0.1.0] - 2026-01-28

### Added

**Core Features**
- Save and load Codex CLI authentication profiles with optional labels
- Interactive profile picker with search and navigation
- List all profiles ordered by last used timestamp
- Delete profiles with confirmation prompts
- Display usage statistics (requests, costs) for profiles
- Automatic OAuth token refresh when loading expired profiles
- Support for both OAuth tokens and API keys

**CLI Experience**
- Terminal styling with color support (respects `NO_COLOR` and `FORCE_COLOR`)
- `--plain` flag to disable all styling and use raw output
- Clear error messages with actionable suggestions
- Command examples in `--help` output

**Installation**
- Smart installer script with automatic OS/architecture detection
- Checksum verification for secure downloads
- Download progress indicators with TTY detection
- Cross-platform support (Linux, macOS, Windows)
- Multiple installation methods: npm, Bun, Cargo, manual script
- Automated update checking with 24-hour interval

**Technical**
- File locking for safe concurrent operations
- Profile storage in `~/.codex/profiles/`
- Atomic file writes to prevent corruption
- 150 tests covering core functionality
- Pre-commit hooks for code quality
- Binary releases for 5 platforms (Linux x64/ARM64, macOS Intel/Apple Silicon, Windows x64)

[Unreleased]: https://github.com/midhunmonachan/codex-profiles/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/midhunmonachan/codex-profiles/compare/v0.3.2...v0.4.0
[0.3.2]: https://github.com/midhunmonachan/codex-profiles/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/midhunmonachan/codex-profiles/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/midhunmonachan/codex-profiles/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/midhunmonachan/codex-profiles/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/midhunmonachan/codex-profiles/releases/tag/v0.1.0
