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
Nested tables do not override these root-level settings, and invalid/unreadable
configuration produces an error instead of silently assuming file storage.

If you choose file-backed storage, configure it explicitly and authenticate again
with the client. Do not change an organization-managed authentication requirement
to make this tool work. File-backed credentials and exports contain secrets.

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

Refresh requests use Codex's client ID, including the nonempty
`CODEX_APP_SERVER_LOGIN_CLIENT_ID` override when configured. Successful refreshes
update `last_refresh` along with returned tokens, matching Codex's cache format.

Live OAuth switching must be verified separately with accounts the tester controls.
Never paste tokens, exported profiles, or `auth.json` into a bug report. Useful
diagnostics are client/tool versions, OS, storage mode, whether `CODEX_HOME` is
customized, the command, and a redacted error.

Source checked September 14, 2026: [OpenAI authentication documentation](https://learn.chatgpt.com/docs/auth).

Implementation cross-check: `openai/codex` revision
[`4e6450bbfd60bdfa845182f30aaa9d6f068e8bbd`](https://github.com/openai/codex/tree/4e6450bbfd60bdfa845182f30aaa9d6f068e8bbd),
specifically `codex-rs/utils/home-dir/src/lib.rs`, `codex-rs/config/src/types.rs`,
`codex-rs/login/src/auth/storage.rs`, `codex-rs/login/src/auth/manager.rs`, and
`codex-rs/protocol/src/auth.rs`. The `_mode` suffix is an internal Rust field;
the public TOML key does not have that suffix. Authentication mode precedence and
the OAuth refresh request fields follow this source. External-host tokens are
ephemeral upstream and are deliberately rejected rather than treated as refreshable OAuth profiles.
