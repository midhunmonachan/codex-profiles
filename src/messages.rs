pub const CANCELLED_MESSAGE: &str = "Cancelled.";

#[cfg(test)]
pub const AUTH_REFRESH_401_TITLE: &str = "Token refresh unauthorized (401)";

#[cfg(test)]
pub const AUTH_RELOGIN_AND_SAVE: &str =
    "Authenticate again with `codex login`, then save this profile";

pub const AUTH_ERR_MISSING_TOKENS: &str = "Error: Missing tokens in {}. Run `codex login`.";
pub const AUTH_ERR_FILE_NOT_FOUND: &str = "Error: Auth file not found. Run `codex login`.";
pub const AUTH_ERR_READ: &str = "Error: Could not read {}: {}";
pub const AUTH_ERR_INVALID_JSON_RELOGIN: &str = "Error: Invalid JSON in {}: {}. Run `codex login`.";
pub const AUTH_ERR_INCOMPLETE_ACCOUNT: &str =
    "Error: Auth file is incomplete (missing account). Run `codex login`.";
pub const AUTH_ERR_INCOMPLETE_EMAIL: &str =
    "Error: Auth file is incomplete (missing email). Run `codex login`.";
pub const AUTH_ERR_INCOMPLETE_PLAN: &str =
    "Error: Auth file is incomplete (missing plan). Run `codex login`.";
pub const AUTH_ERR_PROFILE_MISSING_EMAIL_PLAN: &str =
    "Error: Profile is missing email or plan information.";
pub const AUTH_ERR_PROFILE_MISSING_ACCOUNT: &str = "Error: Profile is missing account information.";
pub const AUTH_ERR_PROFILE_MISSING_ACCESS_TOKEN: &str =
    "Error: Profile is missing an access token.";
pub const AUTH_ERR_PROFILE_NO_REFRESH_TOKEN: &str =
    "Error: This profile has no refresh token. Run `codex login` and save again.";
pub const AUTH_ERR_REFRESH_STATE_CHANGED: &str = "Error: Auth changed on disk. Reload and retry.";
pub const AUTH_ERR_AMBIGUOUS_SAVED_PROFILES: &str = "Error: Active credentials match multiple saved profiles, but none has the same tokens. Save the active account before switching.";
pub const AUTH_ERR_UNSUPPORTED_STORE_MODE: &str = "Error: Codex auth store mode `{}` is not yet supported. codex-profiles requires file-backed auth (cli_auth_credentials_store = \"file\"). No credential-store settings were changed.";
pub const AUTH_ERR_REFRESH_FAILED_OTHER: &str = "Error: Token refresh failed: {}";
pub const AUTH_ERR_INVALID_REFRESH_RESPONSE: &str = "Error: Invalid refresh response: {}";
pub const AUTH_ERR_REFRESH_MISSING_ACCESS_TOKEN: &str =
    "Error: Refresh response is missing an access token.";
pub const AUTH_ERR_INVALID_JSON: &str = "Error: Invalid JSON in {}: {}";
pub const AUTH_ERR_INVALID_JSON_OBJECT: &str = "Error: Invalid JSON in {} (expected object)";
pub const AUTH_ERR_INVALID_TOKENS_OBJECT: &str = "Error: Invalid tokens in {} (expected object)";
pub const AUTH_ERR_WRITE_AUTH: &str = "Error: Could not write {}: {}";

pub const USAGE_UNAVAILABLE_API_KEY_TITLE: &str = "Usage unavailable for API key";
pub const USAGE_UNAVAILABLE_API_KEY_DETAIL: &str =
    "Rate-limit usage data is only available for ChatGPT account profiles.";

#[cfg(test)]
pub const USAGE_UNAVAILABLE_402_TITLE: &str = "Usage unavailable (402)";
pub const USAGE_ERR_SERVICE_UNREACHABLE: &str = "Error: Could not reach usage service: {}";
pub const USAGE_ERR_INVALID_RESPONSE: &str = "Error: Invalid usage response: {}";
pub const USAGE_UNAVAILABLE_DEFAULT: &str = "Data not available";
pub const USAGE_ERR_LOCK_OPEN: &str = "Error: Could not open profiles lock: {}";
pub const USAGE_ERR_LOCK_ACQUIRE: &str =
    "Error: Could not acquire profiles lock. Ensure no other {} is running and retry.";
pub const USAGE_ERR_LOCK_HELD: &str = "Error: Could not lock profiles file: {}";

pub const PROFILE_MSG_SAVED_WITH: &str = "Saved profile {}";
pub const PROFILE_MSG_LOADED_WITH: &str = "Loaded profile {}";
pub const PROFILE_WARN_LOADED_STATUS_FAILED: &str =
    "Loaded profile, but status retrieval failed: {}";
pub const PROFILE_MSG_DELETED_WITH: &str = "Deleted profile {}";
pub const PROFILE_MSG_DELETED_COUNT: &str = "Deleted {} profiles.";
pub const PROFILE_MSG_LABEL_SET: &str = "Set label '{}' for profile {}";
pub const PROFILE_MSG_LABEL_CLEARED: &str = "Cleared label for profile {}";

pub const PROFILE_ERR_SELECTED_INVALID: &str = "Error: Selected profile is invalid: {}";
pub const PROFILE_ERR_FAILED_DELETE: &str = "Error: Failed to delete profile: {}";
pub const PROFILE_ERR_READ_INDEX: &str = "Error: Cannot read profiles index file {}: {}";
pub const PROFILE_ERR_INDEX_INVALID_JSON: &str = "Error: Profiles index file {} is invalid JSON";
pub const PROFILE_ERR_WRITE_INDEX: &str = "Error: Failed to write profiles index file: {}";
pub const PROFILE_ERR_LABEL_EXISTS: &str = "Error: Label '{}' already exists. {}";
pub const PROFILE_ERR_LABEL_NOT_FOUND: &str = "Error: Label '{}' was not found. {}";
pub const PROFILE_ERR_READ_PROFILES_DIR: &str = "Error: Cannot read profiles directory: {}";
pub const PROFILE_ERR_RENAME_PROFILE: &str = "Error: Failed to rename profile {}: {}";
pub const PROFILE_ERR_SYNC_CURRENT: &str = "Error: Failed to sync active profile: {}";
pub const PROFILE_ERR_COPY_CONTEXT: &str = "Error: Failed to {} {}: {}";
pub const PROFILE_COPY_CONTEXT_SAVE: &str = "save profile to";
pub const PROFILE_COPY_CONTEXT_LOAD: &str = "load selected profile to";
pub const PROFILE_ERR_CURRENT_NOT_SAVED: &str = "Error: Active profile is not saved. {}";
pub const PROFILE_WARN_CURRENT_NOT_SAVED_REASON: &str = "Active profile is not saved ({})";
pub const PROFILE_ERR_PROMPT_LOAD: &str = "Error: Could not prompt for load: {}";
pub const PROFILE_ERR_TTY_REQUIRED: &str =
    "Error: {} selection requires a TTY. Run `{} {}` interactively.";
pub const PROFILE_ERR_LABEL_NO_MATCH: &str = "Error: Label '{}' does not match a saved profile. {}";
pub const PROFILE_ERR_ID_NO_MATCH: &str =
    "Error: Profile id '{}' does not match a saved profile. {}";
pub const PROFILE_ERR_DELETE_CONFIRM_REQUIRED: &str =
    "Error: Deletion requires confirmation. Re-run with `--yes` to skip the prompt.";
pub const PROFILE_ERR_PROMPT_DELETE: &str = "Error: Could not prompt for delete: {}";
pub const PROFILE_ERR_PROMPT_CONTEXT: &str = "Error: Could not prompt for {}: {}";
pub const PROFILE_ERR_LABEL_EMPTY: &str = "Error: Label cannot be empty.";
pub const PROFILE_MSG_NOT_FOUND: &str = "Selected profile not found. {}";
pub const PROFILE_ERR_ID_NOT_FOUND: &str = "Error: Profile {} not found";
pub const PROFILE_UNSAVED_NO_MATCH: &str = "no saved profile matches auth.json";

pub const PROFILE_SUMMARY_ERROR: &str = "Error";
pub const PROFILE_SUMMARY_AUTH_ERROR: &str = "Auth error";
pub const PROFILE_SUMMARY_USAGE_ERROR: &str = "Usage error";
pub const PROFILE_SUMMARY_FILE_MISSING: &str = "profile file missing";

pub const PROFILE_PROMPT_SAVE_AND_CONTINUE: &str = "Save active profile and continue";
pub const PROFILE_PROMPT_CONTINUE_WITHOUT_SAVING: &str = "Continue without saving";
pub const PROFILE_PROMPT_CANCEL: &str = "Cancel";
pub const PROFILE_PROMPT_DELETE_ONE: &str = "Delete profile {}? This cannot be undone.";
pub const PROFILE_PROMPT_DELETE_MANY: &str = "Delete {} profiles? This cannot be undone.";
pub const PROFILE_PROMPT_DELETE_SELECTED: &str = "Delete selected profiles? This cannot be undone.";
pub const PROFILE_LOAD_HELP: &str = "Type to search • Use ↑/↓ to select • ENTER to load";
pub const PROFILE_DELETE_HELP: &str =
    "Type to search • Use ↑/↓ to select • SPACE to select • ENTER to delete";

pub const UI_WARNING_PREFIX: &str = "Warning: ";
pub const UI_WARNING_UNSAVED_PROFILE: &str = "Warning: This profile is not saved yet.";
pub const UI_INFO_PREFIX: &str = "Info: {}";
pub const UI_NO_SAVED_PROFILES: &str = "No saved profiles. {}";
pub const UI_HINT_SAVE_PROFILE: &str = "Run {save} to save this profile.";
pub const UI_HINT_LOGIN_AND_SAVE: &str = "Run {login} • then {save}.";
pub const UI_HINT_SAVE_BEFORE_LOADING: &str = "Run {save} before loading.";
pub const UI_HINT_LOGIN_SAVE_BEFORE_LOADING: &str = "Run {login}, then {save} before loading.";
pub const UI_HINT_FORCE_LOAD: &str = "Re-run with {force} to continue without saving.";
pub const UI_HINT_ADD_LABEL_LATER: &str =
    "Add a label later with {label_cmd} for easier later selection.";
pub const UI_HINT_LIST_PROFILES: &str = "Run {list} to see saved profiles.";
pub const UI_NORMALIZED_NOT_LOGGED_IN: &str = "Not logged in. Run `codex login`.";
pub const UI_NORMALIZED_AUTH_INVALID: &str = "Auth file is invalid. Run `codex login`.";
pub const UI_NORMALIZED_AUTH_INCOMPLETE: &str = "Auth is incomplete. Run `codex login`.";
pub const UI_ERROR_PREFIX: &str = "Error: ";
pub const UI_ERROR_TWO_LINE: &str = "Error: {}\n{}";
pub const UI_UNKNOWN_PROFILE: &str = "Unknown profile{}";

pub const CMD_ERR_UPDATE_RUN: &str = "Error: Could not run update command: {}";
pub const CMD_ERR_UPDATE_FAILED: &str = "Error: Update command failed: {}";

pub const COMMON_ERR_RESOLVE_HOME: &str = "Error: Could not resolve home directory";
pub const COMMON_ERR_EXISTS_NOT_DIR: &str = "Error: {} exists and is not a directory";
pub const COMMON_ERR_CREATE_PROFILES_DIR: &str = "Error: Cannot create profiles directory {}: {}";
#[cfg(unix)]
pub const COMMON_ERR_SET_PERMISSIONS: &str = "Error: Cannot set permissions on {}: {}";
pub const COMMON_ERR_WRITE_LOCK_FILE: &str = "Error: Cannot write profiles lock file {}: {}";
pub const COMMON_ERR_RESOLVE_PARENT: &str = "Error: Cannot resolve parent directory for {}";
pub const COMMON_ERR_CREATE_DIR: &str = "Error: Cannot create directory {}: {}";
pub const COMMON_ERR_INVALID_FILE_NAME: &str = "Error: Invalid file name {}";
pub const COMMON_ERR_GET_TIME: &str = "Error: Could not determine temporary file timestamp: {}";
pub const COMMON_ERR_CREATE_TEMP: &str = "Error: Failed to create temp file for {}: {}";
pub const COMMON_ERR_WRITE_TEMP: &str = "Error: Failed to write temp file for {}: {}";
pub const COMMON_ERR_SET_TEMP_PERMISSIONS: &str =
    "Error: Failed to set temp file permissions for {}: {}";
pub const COMMON_ERR_REPLACE_FILE: &str = "Error: Failed to replace {}: {}";
pub const COMMON_ERR_READ_METADATA: &str = "Error: Failed to read metadata for {}: {}";
pub const COMMON_ERR_READ_FILE: &str = "Error: Failed to read {}: {}";
pub const COMMON_ERR_EXISTS_NOT_FILE: &str = "Error: {} exists and is not a file";

pub const UPDATE_TITLE_AVAILABLE: &str = "Update available!";
pub const UPDATE_RELEASE_NOTES: &str = "Release notes: {}\n";
pub const UPDATE_OPTION_NOW: &str = "1) Update now (runs `{}`)\n";
pub const UPDATE_OPTION_SKIP: &str = "2) Skip\n";
pub const UPDATE_OPTION_SKIP_VERSION: &str = "3) Skip until next version\n";
pub const UPDATE_PROMPT_SELECT: &str = "Select [1-3]: ";
pub const UPDATE_NON_TTY_RUN: &str = "Run `{}` to update.\n";
pub const UPDATE_ERR_READ_CHOICE: &str = "Error: Could not read update choice: {}";
pub const UPDATE_ERR_SHOW_PROMPT: &str = "Error: Could not show update prompt: {}";
pub const UPDATE_ERR_PERSIST_DISMISSAL: &str = "Failed to persist update dismissal: {}\n";
pub const UPDATE_ERR_REFRESH_VERSION: &str = "Failed to update version: {}";

pub fn msg1(template: &str, a: impl std::fmt::Display) -> String {
    template.replacen("{}", &a.to_string(), 1)
}

pub fn msg2(template: &str, a: impl std::fmt::Display, b: impl std::fmt::Display) -> String {
    let out = template.replacen("{}", &a.to_string(), 1);
    out.replacen("{}", &b.to_string(), 1)
}

pub fn msg3(
    template: &str,
    a: impl std::fmt::Display,
    b: impl std::fmt::Display,
    c: impl std::fmt::Display,
) -> String {
    let out = template.replacen("{}", &a.to_string(), 1);
    let out = out.replacen("{}", &b.to_string(), 1);
    out.replacen("{}", &c.to_string(), 1)
}
