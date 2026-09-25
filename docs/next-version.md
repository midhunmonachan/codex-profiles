# Next-version research and release scope

Reviewed September 24, 2026. Project baseline: [`879e538`](https://github.com/midhunmonachan/codex-profiles/commit/879e538ca3d007768fe1c710add0f8ee573d918d), after v0.3.2.
Upstream comparison: [`openai/codex` at `a33fb975`](https://github.com/openai/codex/tree/a33fb9751c1e468b0062aae63e8f06e9d75e2376).

The next release combines account-switching reliability with clearer usage
reporting. It keeps account credentials separate from Codex configuration.

## Findings and implementation priorities

| Priority | Finding at the baseline | Release direction |
| --- | --- | --- |
| P1 | `load` warns and continues when preserving the active account fails. | Require successful preservation before replacing active credentials. |
| P1 | A refresh checks disk state before the network call, then merges its response into whatever file exists afterward. | Serialize this tool's credential changes and recheck the complete authentication snapshot before persisting a response. |
| P1 | Selected-profile status can refresh the saved copy of the active account while leaving `auth.json` stale. | Use the active credential path when the selected account is active, then synchronize refreshes only to the exact matching saved alias; allow a sole alias to absorb rotation and fail closed for ambiguous aliases. |
| P1 | Usage sorts windows and calls the first five-hour and the second weekly, regardless of actual duration. | Preserve upstream primary/secondary positions and actual durations; retain duration-specific JSON aliases only when they match. |
| P2 | An empty usage payload is reported as JSON success with no usage data. | Report `unavailable` explicitly. |
| P2 | A server's arbitrary `Retry-After` value can suspend `status` for hours. | Return the server error when its requested delay exceeds the interactive retry budget. Never retry before the requested time. |
| P2 | Users need a quicker way to compare multiple accounts and usage buckets. | Add an opt-in compact view to `status --all`, reusing its existing requests and refresh behavior. |
| P2 | Configuration and credential-store incompatibilities are difficult to distinguish from account problems. | Add local, read-only configuration diagnostics to `doctor`, with redacted output. |
| P2 | Ordinary mutations can replace a malformed profile index and discard recoverable labels. | Preserve malformed metadata and direct the user to the existing explicit `doctor --fix` recovery path. |
| P2 | Failed atomic writes can leave temporary credential files, and a Windows retry removes the old destination before proving replacement will succeed. | Clean temporary files on all failure paths and retain the original destination on replacement failure. |
| P2 | Release verification compares checksum metadata without independently hashing artifacts; native archives can lose executable permissions. | Hash actual artifacts, validate archive contents and permissions, and exercise tampering and packaging regressions. |

Refresh protection is cooperative between Codex Profiles processes. Codex itself
does not acquire this tool's profile lock. Continue to finish active client work
before switching, and restart the affected client afterward; tests with synthetic
credentials do not establish live desktop or keyring compatibility.

The September 24 review found no change to the file-backed auth, token,
home-directory, or rate-limit contracts consumed by this release. Current Codex
also supports newer authentication modes such as agent identity, personal access
tokens, externally provided headers, and Bedrock credentials; this project
continues to reject those modes before reading or writing a profile because their
storage and refresh lifecycles are outside its supported boundary.

## Upstream contracts

- Authentication can use `file`, `keyring`, `auto`, or `ephemeral` storage. This
  project supports file-backed credentials and must refuse unsupported active
  storage. Encrypted secrets are an upstream backend implementation detail, not
  another public credential-store setting. See [authentication guidance](https://learn.chatgpt.com/docs/auth)
  and the pinned [storage implementation](https://github.com/openai/codex/blob/94174e44cbc54cece45f6052328ca0c2cd7a8a2a/codex-rs/login/src/auth/storage.rs).
- OAuth refresh belongs to managed ChatGPT authentication. External auth modes
  and other providers have different lifecycles. Preserve unknown auth fields
  and reject unsupported modes rather than treating them as managed OAuth. See
  the pinned [authentication manager](https://github.com/openai/codex/blob/94174e44cbc54cece45f6052328ca0c2cd7a8a2a/codex-rs/login/src/auth/manager.rs).
- Rate limits consist of buckets and primary/secondary windows. Window duration
  and reset time come from the service. The public examples include 15-minute
  and 60-minute windows. See [app-server rate-limit documentation](https://learn.chatgpt.com/docs/app-server)
  and the pinned [backend mapping](https://github.com/openai/codex/blob/94174e44cbc54cece45f6052328ca0c2cd7a8a2a/codex-rs/backend-client/src/client.rs).
- Native `codex --profile NAME` selects a separate `$CODEX_HOME/NAME.config.toml`
  layer in current Codex. It does not select a saved account. This tool's
  `save`/`load` commands manage credentials. See [advanced configuration](https://learn.chatgpt.com/docs/config-file/config-advanced).
- Imported bundles use the same explicit `auth_mode` precedence as active auth
  files and reject unsupported external credential modes before writing imported
  data.
- Upstream source contains account-session protocol types, but at the reviewed
  revision the corresponding methods are not registered as callable app-server
  requests. Do not build a switching feature on dormant types. See the pinned
  [request registry](https://github.com/openai/codex/blob/94174e44cbc54cece45f6052328ca0c2cd7a8a2a/codex-rs/app-server-protocol/src/protocol/common.rs).

## Product evidence and deferred work

[Issue #22](https://github.com/midhunmonachan/codex-profiles/issues/22) identifies
the need for usage and reset times at a glance. Its discussion favors a compact
status view grouped by account and bucket, while keeping `list` local and fast.
The existing `load --with-status` feature remains the immediate post-switch view.

[Config snapshots (#25)](https://github.com/midhunmonachan/codex-profiles/issues/25)
and [encrypted exports (#20)](https://github.com/midhunmonachan/codex-profiles/issues/20)
remain separate design work. Config snapshots need clear ownership, restoration,
and conflict behavior. Encryption needs a reviewed format, key derivation,
password handling, migration, and recovery design. Neither is necessary to ship
the confirmed reliability fixes and compact status view.

Also deferred: direct keyring support, a persistent app-server integration, a GUI,
and changes to the persisted account identity schema. The duplicate-alias guard
resolves the existing composite identity against exact token snapshots without
changing stored identity fields. Schema changes require dedicated compatibility
fixtures and migration design.

## Verification boundary

Use synthetic auth fixtures and loopback HTTP servers for refresh, switching,
rate-limit parsing, malformed responses, and concurrency tests. No test should
use the developer's real `auth.json`, perform a real login/logout, or contact the
live usage service. Run formatting, Clippy, the Rust suite, security audit,
Rust line-coverage reporting, and package smoke checks before release. Unix terminal
tests use Python 3's standard-library pseudo-terminal support to drive real
prompts, including cancellation and concurrent filesystem changes. Cross-platform CI and a
controlled live-account check remain distinct from local Linux verification.
