# Client compatibility

Codex Profiles manages file-backed Codex credentials. It is not a browser-session
switcher and does not promise to switch every account surface in the ChatGPT desktop app.

## Storage location

`CODEX_HOME`, when nonempty, selects the Codex directory directly. Authentication
is read from `CODEX_HOME/auth.json`, configuration from `CODEX_HOME/config.toml`,
and saved profiles from `CODEX_HOME/profiles/`. Use the same value for Codex and
Codex Profiles. Changing this location does not move existing profiles.

Without `CODEX_HOME`, the default is `~/.codex`. The legacy `CODEX_PROFILES_HOME`
override still selects a home directory, with `.codex` appended. `CODEX_HOME`
takes precedence when both overrides are present.

Like Codex itself, an explicit `CODEX_HOME` must already exist and be a directory;
it is canonicalized before use. A missing path is an error, not a new empty profile store.

For example, in PowerShell:

```powershell
$env:CODEX_HOME = 'C:\Users\YourName\codex-work'
New-Item -ItemType Directory -Path $env:CODEX_HOME -Force
codex login
codex-profiles save --label work
```

## Credential stores

The documented root-level setting is `cli_auth_credentials_store`:

| Mode | Codex Profiles behavior |
| --- | --- |
| `file` or absent | Manage `auth.json` and saved profiles. |
| `keyring` | Refuse active-file reads/switches; OS credential storage is not implemented. |
| `auto` | Refuse active-file reads/switches because the OS store may take precedence over a stale file. |
| `ephemeral` | Refuse active-file reads/switches; process-memory credentials cannot be switched by copying a file. |

`load --force` does not bypass this check. No credential-store settings are changed.
The old `cli_auth_credentials_store_mode` spelling is recognized only as a fallback
for configurations written against earlier Codex Profiles guidance. Use the documented
setting for Codex itself. The documented key takes precedence over the fallback.
Imported bundles use the same auth-mode precedence as active auth files: an explicit
non-null `auth_mode` wins, followed by Codex's legacy credential fields, then the
API-key or managed ChatGPT defaults. An explicit supported mode can override legacy
fields, while unsupported authentication modes and externally managed-only
credential fields are rejected before imported files or index metadata are written.
Nested tables do not override these root-level settings, and invalid/unreadable
configuration produces an error instead of silently assuming file storage.

If you choose file-backed storage, configure it explicitly and authenticate again
with the client. Do not change an organization-managed authentication requirement
to make this tool work. File-backed credentials and exports contain secrets.

`doctor` includes a read-only `codex_config` check for the root user configuration:
credential-store mode, usage endpoint compatibility, and legacy native profile
settings. It omits configuration values that might contain secrets and makes no
network request. `doctor --fix` repairs profile storage; it does not edit Codex
configuration. This check does not resolve managed requirements, environment
authentication, per-invocation CLI overrides, or Codex's full configuration stack.

## Account profiles and native configuration profiles

`codex-profiles save` and `load` manage saved account credentials. They do not
save or replace `config.toml`, provider settings, hooks, or project configuration.

Native `codex --profile NAME` selects `$CODEX_HOME/NAME.config.toml` as a
configuration layer. In Codex 0.134.0 and later, selecting `NAME` rejects a
matching legacy `[profiles.NAME]` table or root `profile = "NAME"` selector.
Unrelated legacy profile tables may remain, but migrate settings to the top
level of the separate profile file. Account selection and configuration
selection remain independent. See [OpenAI's configuration guidance](https://learn.chatgpt.com/docs/config-file/config-advanced).

## CLI, IDE, and desktop

- The CLI is the primary supported client. Confirm the selected login with
  `codex login status` using the same `CODEX_HOME`.
- The CLI and IDE extension share cached login details according to OpenAI's
  documentation; running clients may keep credentials in memory.
- Finish active work before switching. Restart the affected client afterward and
  verify its displayed account/workspace. A successful file copy does not prove a
  running desktop client changed accounts.
- The ChatGPT desktop app supports multiple product surfaces. This tool does not
  modify browser cookies, desktop session databases, OS keyrings, or packaged-app
  redirected storage. The app's name alone does not establish storage compatibility.
- API-key files are supported. Browser sessions, access-token/workload-identity
  workflows, and third-party provider configuration switching are not covered by
  this compatibility update.

## Usage and refresh

`status` uses a client usage endpoint, which can change independently of this tool.
An HTTP or parsing failure should be reported as unavailable/error, not treated as
zero usage. Existing tests cover multiple usage buckets, token refresh, and changed
on-disk authentication; these use synthetic credentials and loopback servers.

`status --all --compact` is a human-readable summary grouped by account and usage
bucket. It uses the same requests and refresh path as normal status. `list` remains
a local operation without usage lookups.

Status JSON exposes `usage.buckets[].primary` and `.secondary` with
`left_percent`, `reset_at` (Unix seconds), and `window_seconds`. Their order matches
the service response. The existing `five_hour` and `weekly` fields are aliases
for exact 18,000-second and 604,800-second windows; either can be null. Consumers
should use primary/secondary and durations for custom limits. An empty response
has `usage.state = "unavailable"`. HTTP errors retain `usage.state = "error"`
and the existing top-level `error` object.

Usage retries are bounded. If `Retry-After` requests more than three seconds,
status reports the server response immediately instead of blocking or retrying
before the requested time.

Refresh requests use Codex's client ID, including the nonempty
`CODEX_APP_SERVER_LOGIN_CLIENT_ID` override when configured. Successful refreshes
update `last_refresh` along with returned tokens, matching Codex's cache format.
The upstream `CODEX_REFRESH_TOKEN_URL_OVERRIDE` is also honored; configure it only
for an endpoint you trust to receive refresh tokens.

Refresh keeps the stored `account_id` as the account identity, matching Codex's
pinned account behavior. A newly returned `id_token` may contain an account claim,
but that claim cannot silently select a different saved account.

Refresh and switching operations cooperate through the profile lock and check for
authentication changes before writing. A changed account or auth mode must not
receive a stale refresh response. When multiple saved files share the same
composite identity, synchronization prefers the saved profile whose complete
token snapshot matches the active auth file. A sole alias may absorb token
rotation; if multiple aliases have no exact match, a load fails closed with
save/reload guidance, while status keeps the active account unsaved instead of
overwriting an arbitrary alias. A selected active account is refreshed through
its active auth file and synchronized only to that verified saved profile. The
lock is specific to Codex Profiles; active Codex clients do not participate, so
finishing active work before switching remains necessary.

Live OAuth switching must be verified separately with accounts the tester controls.
Never paste tokens, exported profiles, or `auth.json` into a bug report. Useful
diagnostics are client/tool versions, OS, storage mode, whether `CODEX_HOME` is
customized, the command, and a redacted error.

Source checked September 22, 2026: [OpenAI authentication documentation](https://learn.chatgpt.com/docs/auth)
and [app-server rate-limit documentation](https://learn.chatgpt.com/docs/app-server).

Implementation cross-check: `openai/codex` revision
[`94174e44cbc54cece45f6052328ca0c2cd7a8a2a`](https://github.com/openai/codex/tree/94174e44cbc54cece45f6052328ca0c2cd7a8a2a),
specifically `codex-rs/utils/home-dir/src/lib.rs`, `codex-rs/config/src/types.rs`,
`codex-rs/login/src/auth/storage.rs`, `codex-rs/login/src/auth/manager.rs`, and
`codex-rs/protocol/src/auth.rs`, and `codex-rs/backend-client/src/client.rs`.
The `_mode` suffix is an internal Rust field;
the public TOML key does not have that suffix. Authentication mode precedence and
the OAuth refresh request fields follow this source. External-host tokens are
ephemeral upstream and are deliberately rejected rather than treated as refreshable OAuth profiles.
