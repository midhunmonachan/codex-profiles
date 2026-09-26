# Optional config snapshots: design proposal

Status: **design for discussion; not implemented**. Based on v0.4.0 plus
[`f3b1730`](https://github.com/midhunmonachan/codex-profiles/commit/f3b1730106afa489a9e7fd2b33a81b7fa3a87445).
The feature remains open for contribution through
[issue #25](https://github.com/midhunmonachan/codex-profiles/issues/25).
This document distinguishes the agreed direction from implementation proposals;
it does not assign the work or finalize the choices below.

## Goal and agreed direction

Switching only `auth.json` can leave an account paired with configuration intended
for another provider. An optional snapshot would let a saved account restore its
associated root `config.toml` as well.

The [maintainer's scope](https://github.com/midhunmonachan/codex-profiles/issues/25#issuecomment-5840326121)
is:

- Auth-only save/load remains the default. Existing profiles retain that behavior.
- Proposed `save --include-config` opts into a snapshot. Loading a profile with a
  snapshot restores both files; loading an auth-only profile leaves config alone.
- Preserve existing profiles, `CODEX_HOME` resolution, and credential-store refusals.
- Validate before replacing files and provide recovery for failed or interrupted
  restores. Never report success while silently leaving a mismatched pair.
- Include user documentation and meaningful regression tests.

## Codex boundary

The snapshot scope is `$CODEX_HOME/config.toml`, alongside the supported
file-backed `auth.json`. Native `codex --profile NAME` selects a separate
`NAME.config.toml` configuration layer; it does not select an account. Snapshotting
the root file does not capture native profile files, project configuration,
managed requirements, environment variables, or invocation overrides. It is not
a complete effective-configuration backup. See [OpenAI's configuration guidance](https://learn.chatgpt.com/docs/config-file/config-advanced)
and this project's [compatibility limits](compatibility.md).

Preserve the existing refusals for `keyring`, `auto`, `ephemeral`, and unsupported
auth modes. Check both the current root configuration and the proposed restored
configuration before credential mutation; copying a file cannot make an
unsupported credential lifecycle work. Do not rewrite managed requirements or
claim to resolve Codex's full policy/configuration stack. Supporting additional
provider authentication is separate work. [OpenAI authentication guidance](https://learn.chatgpt.com/docs/auth)

Config can contain secrets, endpoints, and commands. Proposed snapshots preserve
the original bytes, including comments and unknown settings, after TOML validation.
Saving, importing, inspecting, or restoring a snapshot must not execute its commands
or print its contents. Restrict access to snapshots and recovery copies as for
credentials. Parsing valid TOML does not establish that its settings are trusted
or usable in another machine's environment.

## Current implementation constraints

| Surface | Evidence and implication |
| --- | --- |
| Command dispatch | [cli.rs](../src/cli.rs) and `run_with_update_outcome` in [lib.rs](../src/lib.rs) currently pass no snapshot policy. Proposed flags must have explicit help, JSON behavior, and noninteractive semantics. |
| Saved profiles | `profile_path_for_id` in [profiles.rs](../src/profiles.rs) uses `profiles/<id>.json`. The top-level scanner treats other `.json` files as auth profiles except reserved metadata filenames. Snapshot records must not collide with that scanner. |
| Profile identity | `resolve_save_id` can call `rename_profile_id`. Bind a snapshot to its profile record and move the association when its ID changes; labels are aliases, not storage keys. |
| Index and old clients | `ProfilesIndex` and its entries do not retain unknown fields. Older writers and index repair can erase metadata they do not understand. Existing-profile upgrade support does not imply safe mixed-version writers. |
| Active restore | `load_profile` prompts, reacquires the store lock, revalidates active auth and selected tokens, preserves current auth, copies selected auth, then saves metadata. Extend validation to config presence/bytes and the selected snapshot generation. |
| File replacement | `write_atomic_with_permissions` in [common.rs](../src/common.rs) syncs a temporary file and renames it. It does not sync the containing directory. Two such calls do not form a two-file transaction. |
| Cooperative lock | `ProfileStore` and `lock_usage` in [usage.rs](../src/usage.rs) coordinate this tool's writes and refreshes. Codex and unrelated editors do not participate. |
| Repair and transfer | [doctor.rs](../src/doctor.rs) repairs storage/index, never active config. Export/import currently use version 1 auth-only bundles. Snapshot recovery and transfer need explicit contracts. |

## Storage proposal to evaluate

Prefer retaining existing auth paths and placing snapshots plus versioned
descriptors in a separate private area under `profiles/`. This minimizes changes
to existing auth-only stores. A descriptor must distinguish an auth-only profile
from a profile whose declared snapshot is missing or damaged. Record a snapshot
generation and integrity information so stale copies cannot be silently combined.
Checksums detect inconsistency; they do not authenticate a malicious local writer.

An alternative is a per-profile directory containing auth, config, and metadata.
It groups saved state but changes profile discovery, path resolution, and migration.
Neither layout makes replacement of the two active files atomic. The contributor
should compare these options and propose exact paths, descriptor/schema rules,
save/rename commit boundaries, and reconstruction behavior before coding.

Do not make a new optional index field the sole record of snapshot ownership.
Specify how missing descriptors, orphan snapshots, index rebuilds, and older
clients are handled. No downgrade guarantee is selected by this proposal.

## Proposed restore and recovery model

Use a recoverable transaction for a snapshot-aware restore. This is a proposal
for review, not a promise that another process can only ever observe a complete
pair. Require users to finish active Codex work before switching, then restart
and verify the selected account as described in [compatibility.md](compatibility.md).

1. **Validate:** obtain any confirmation, acquire the existing profile lock, check
   pending recovery, and revalidate both current files and the selected saved
   generation. Distinguish absence from unreadability. Validate auth, TOML,
   credential-store restrictions, and paths before replacing anything. Preserve
   current auth using the existing exact-alias rules; config preservation follows
   the overwrite decision below. Avoid holding the lock while prompting or waiting
   on network requests.
2. **Prepare:** persist private recovery material and a versioned transaction
   record before touching either active file. Include original file presence,
   original bytes, desired bytes or immutable saved-generation references,
   identities/hashes, and the phase. Never put credential contents in diagnostics.
3. **Apply:** replace files individually in the selected order, rechecking for
   external changes and recording progress. Recovery must recognize a replacement
   that succeeded just before its progress record could be written.
4. **Commit the pair:** verify both intended files, then persist the pair commit
   marker. Update profile metadata idempotently afterward. If metadata fails,
   report that the pair was restored and metadata recovery remains pending.
5. **Finish:** remove only recovery material owned by the completed transaction.
   Pending/conflicted records remain available for recovery, with private access.

A candidate recovery policy restores before-images before the pair commit marker
and completes metadata/cleanup afterward. Only act automatically when current
files match recognized transaction states. Unexpected bytes, including a Codex
token refresh, are a conflict: preserve them and the recovery material, report the
conflict, and stop automatic replacement. Recovery itself must survive interruption.
Rollback versus roll-forward and explicit versus automatic recovery remain open.

Every participating path that can write credentials or snapshot metadata must
check pending recovery, including auth-only mutations and status-triggered token
refresh. Read-only inspection should report pending state. Plain `doctor` must
remain diagnostic; introducing config-writing recovery through an explicit command
or flag needs agreement and updated help. Do not silently expand `doctor --fix`.

Two replacements expose an intermediate mixed pair, and content rechecks still
leave a race with nonparticipating writers. A journal cannot eliminate either.
Process interruption and machine power loss are different guarantees: the latter
needs platform-specific file/directory synchronization and evidence. Agree on the
promised durability level and supported filesystems before calling this crash-safe.

## Decisions needed before implementation

| Decision | Proposed starting point / question to settle |
| --- | --- |
| Storage and migration | Compare separate snapshot storage with per-profile directories; choose exact schema, generation/ownership rules, rename recovery, and old-client/downgrade policy. |
| Missing config at save | Prefer an error for `--include-config` when config is absent. Alternatively, capturing deliberate absence would authorize deleting active config on restore and needs explicit semantics. Unreadable config is always an error. |
| Re-saving a profile | Prefer preserving an existing snapshot on ordinary auth-only resave and updating it only with `--include-config`. Decide how a snapshot is explicitly removed. Automatic auth refresh must not capture edited config. |
| Overwriting active config | Decide how to recognize/preserve local edits, what confirmation is required, and how noninteractive callers opt in. Existing `load --force` only skips saving unsaved auth; it must not silently become a config-overwrite or store-policy bypass. |
| Recovery | Choose file replacement order, journal location/retention, rollback or roll-forward, explicit recovery UX, external-write conflict handling, and process-crash versus power-loss guarantees. |
| Export/import | Choose a versioned snapshot-aware bundle and explicit consent to export/import configuration, or a clearly limited first milestone. Version 1 auth-only bundles stay importable. Never silently claim a complete backup while dropping snapshots. Imported config remains untrusted. |

These are contributor/maintainer review questions, not implementation defaults
already approved by this document. Resolve them together in issue #25 before a
feature PR. Keep encrypted exports ([#20](https://github.com/midhunmonachan/codex-profiles/issues/20))
as a separate format/security design.

## Acceptance criteria

The implementation proposal must map each applicable case to an observable test;
this table is a test plan, not evidence of implemented or passing snapshot tests.

| Scenario | Required observation |
| --- | --- |
| Existing profiles and version 1 bundles | Existing IDs, labels, auth contents, and auth-only behavior survive upgrade. Ordinary auth-only load leaves the active config bytes/presence unchanged. |
| Explicit snapshot save | Auth and config belong to the same validated save operation. Unknown settings/comments round-trip without being displayed. Missing/unreadable config follows the agreed rule, with no partial saved generation on error. |
| Re-save and ID/label changes | The selected preserve/update/remove rule is tested. ID renames carry snapshot ownership; label edits do not attach another profile's config. Duplicate-identity aliases can retain different snapshots. |
| Load validation/cancellation | Invalid auth/TOML, unsupported stores/modes in current or target state, missing/corrupt declared snapshots, cancellation, and changed files while prompting stop before active replacement. `--force` cannot bypass these checks. |
| Snapshot-aware load | The intended pair is restored and success is reported only after the required commit boundary. A failed optional `--with-status` lookup does not misreport the restore outcome. |
| Concurrent refresh or edits | Two tool processes coordinate; test revalidation against active/selected config changes and auth rotation. Detected external changes enter conflict and are not overwritten by blind rollback. Do not promise detection of every nonparticipating write between checks and replacement. |
| Interruptions and I/O errors | Inject failures and terminate the process around prepare, each replacement, commit, metadata, recovery, and cleanup. The next invocation can diagnose/recover recognized states, including a replacement preceding its journal update. Recovery is repeatable; evidence is retained when cleanup or rollback fails. |
| Save/delete/rename failures | Saved auth, snapshot, descriptor, and index cannot silently disagree. Cancelled deletion preserves all files; successful deletion removes only selected profiles' owned snapshots. Partial failure is reported with a defined recovery path. |
| Token refresh | Rotated auth is synchronized only to the verified saved alias. Its snapshot bytes/generation are untouched, and pending restore recovery blocks conflicting writes. |
| Doctor and metadata repair | Plain doctor reports pending/orphan/missing snapshots without writes. Index repair preserves known ownership or reports uncertainty; it never guesses a config or turns snapshot-aware state into auth-only state silently. |
| Transfer and older clients | Test the agreed format/consent rules, old auth-only imports, rejection of unknown versions, duplicates/unsafe paths, and rollback of partial import writes. Verify any promised downgrade behavior with an actual older binary in an isolated fixture. |
| Privacy and filesystem boundaries | Synthetic secrets never appear in logs, JSON output, or errors. Private permissions apply before writes become visible; symlinks/reparse points, directories, traversal, and case-insensitive collisions cannot redirect writes or cleanup outside the owned store. Test platform-specific permission behavior explicitly. |

Use temporary Codex homes, synthetic credentials, controlled concurrency, and
real file/process failure boundaries on Linux, macOS, and Windows. Run the
repository's normal formatting, lint, security, test, and coverage-report checks;
there is no coverage-percentage requirement. Live account/client behavior is a
separate controlled check and must not use contributor credentials in CI.

The contributor's next deliverable is a short proposal resolving the decision
table, the storage/transaction formats, and the test mapping. Maintainer review
precedes implementation; contributor changes follow the normal PR workflow.
