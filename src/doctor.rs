use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::path::Path;

use serde::Serialize;

use crate::{
    InstallSource, Paths, current_saved_id, detect_install_source, is_profile_ready, lock_usage,
    print_output_block, profile_files, profile_id_from_path, read_tokens, repair_profiles_metadata,
};

const CONFIG_SCOPE_CAVEAT: &str =
    "user config only; managed requirements and CLI/environment overrides are not evaluated";

#[derive(Clone, Copy, Debug, Default)]
enum Level {
    Ok,
    Warn,
    Error,
    #[default]
    Info,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Error => "error",
            Level::Info => "info",
        }
    }
}

#[derive(Default, Serialize)]
struct Counts {
    ok: usize,
    warn: usize,
    error: usize,
    info: usize,
}

impl Counts {
    fn add(&mut self, level: Level) {
        match level {
            Level::Ok => self.ok += 1,
            Level::Warn => self.warn += 1,
            Level::Error => self.error += 1,
            Level::Info => self.info += 1,
        }
    }
}

struct Check {
    level: Level,
    name: &'static str,
    detail: String,
}

#[derive(Serialize)]
struct DoctorCheckJson {
    name: &'static str,
    level: &'static str,
    detail: String,
}

#[derive(Serialize)]
struct DoctorJson {
    checks: Vec<DoctorCheckJson>,
    summary: Counts,
    #[serde(skip_serializing_if = "Option::is_none")]
    repairs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Check {
    fn new(level: Level, name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            level,
            name,
            detail: detail.into(),
        }
    }

    fn render(&self) -> String {
        format!("[{}] {}: {}", self.level.label(), self.name, self.detail)
    }
}

enum AuthState {
    Missing,
    Valid,
    Incomplete(String),
    Invalid(String),
}

struct SavedProfilesReport {
    check: Check,
    tokens: BTreeMap<String, Result<crate::Tokens, String>>,
}

pub fn doctor(paths: &Paths, fix: bool, json: bool) -> Result<(), String> {
    let repairs = if fix {
        match repair(paths) {
            Ok(repairs) => Some(repairs),
            Err(err) => {
                if json {
                    let checks = collect_checks(paths);
                    let counts = summarize_checks(&checks);
                    return print_doctor_json(checks, counts, Some(Vec::new()), Some(err));
                }
                return Err(err);
            }
        }
    } else {
        None
    };
    let checks = collect_checks(paths);
    let counts = summarize_checks(&checks);

    if json {
        return print_doctor_json(checks, counts, repairs, None);
    }

    let mut lines = vec!["Doctor".to_string(), String::new()];

    for check in checks {
        lines.push(check.render());
    }

    lines.push(String::new());
    lines.push(format!(
        "Summary: {} ok, {} warn, {} error, {} info",
        counts.ok, counts.warn, counts.error, counts.info
    ));
    if let Some(repairs) = repairs {
        lines.push(String::new());
        if repairs.is_empty() {
            lines.push("No repairs needed.".to_string());
        } else {
            lines.push("Repairs applied:".to_string());
            for repair in repairs {
                lines.push(format!("- {repair}"));
            }
        }
    }
    print_output_block(&lines.join("\n"));
    Ok(())
}

fn repair(paths: &Paths) -> Result<Vec<String>, String> {
    let mut repairs = repair_storage(paths)?;
    repairs.extend(repair_profiles_metadata(paths)?);
    Ok(repairs)
}

fn repair_storage(paths: &Paths) -> Result<Vec<String>, String> {
    let mut repairs = Vec::new();

    if paths.profiles.exists() {
        if !paths.profiles.is_dir() {
            return Err("Error: profiles directory exists but is not a directory".to_string());
        }
    } else {
        create_profiles_directory(&paths.profiles)?;
        #[cfg(unix)]
        {
            set_path_mode(&paths.profiles, 0o700)?;
        }
        repairs.push("Created profiles directory".to_string());
    }

    if paths.profiles_index.exists() && !paths.profiles_index.is_file() {
        return Err("Error: profiles index exists but is not a file".to_string());
    }

    if paths.profiles_lock.exists() {
        if !paths.profiles_lock.is_file() {
            return Err("Error: profiles lock exists but is not a file".to_string());
        }
    } else {
        create_profiles_lock(&paths.profiles_lock)?;
        repairs.push("Created profiles lock file".to_string());
    }

    repairs.extend(repair_storage_permissions(paths)?);

    Ok(repairs)
}

fn collect_checks(paths: &Paths) -> Vec<Check> {
    let auth = auth_state(paths);
    let mut checks: Vec<Check> = inspect_install(paths).into_iter().collect();
    checks.push(inspect_codex_config(paths));
    checks.push(inspect_auth(paths, &auth));
    checks.push(inspect_profiles_dir(paths));
    checks.push(inspect_profiles_index(paths));
    checks.push(inspect_profiles_lock(paths));

    let saved = inspect_saved_profiles(paths);
    checks.push(saved.check);
    checks.push(inspect_current_profile(paths, &auth, &saved.tokens));
    checks
}

/// Inspect only the root Codex user configuration needed to decide whether
/// codex-profiles can safely operate on the active auth store.  This check is
/// deliberately conservative: it never reports TOML values, it does not try
/// to resolve the complete Codex configuration stack, and it never repairs
/// config.toml (including when doctor runs with --fix).
fn inspect_codex_config(paths: &Paths) -> Check {
    let config_path = paths.codex.join("config.toml");
    let contents = match fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Check::new(
                Level::Ok,
                "codex_config",
                format!(
                    "missing; Codex defaults apply (auth storage=file; ChatGPT endpoint=official default); {CONFIG_SCOPE_CAVEAT}"
                ),
            );
        }
        Err(_) => {
            return Check::new(
                Level::Error,
                "codex_config",
                format!(
                    "unreadable; auth storage and endpoint compatibility are unknown; {CONFIG_SCOPE_CAVEAT}"
                ),
            );
        }
    };

    let root: toml::Table = match contents.parse() {
        Ok(root) => root,
        Err(_) => {
            return Check::new(
                Level::Error,
                "codex_config",
                format!(
                    "invalid TOML; auth storage and endpoint compatibility are unknown; configuration contents were omitted; {CONFIG_SCOPE_CAVEAT}"
                ),
            );
        }
    };

    let mut level = Level::Ok;
    let mut details = vec!["valid".to_string()];

    let storage = inspect_config_auth_storage(&root);
    level = level_max(level, storage.level);
    details.push(storage.detail);

    match inspect_config_endpoint(paths, &root) {
        EndpointState::OfficialDefault => {
            details.push("ChatGPT endpoint=official default".to_string())
        }
        EndpointState::Official => details.push("ChatGPT endpoint=official host".to_string()),
        EndpointState::Loopback => {
            level = level_max(level, Level::Warn);
            details.push("ChatGPT endpoint=loopback host".to_string());
        }
        EndpointState::Unrecognized => {
            level = level_max(level, Level::Warn);
            details.push("ChatGPT endpoint=unrecognized host".to_string());
        }
        EndpointState::Invalid => {
            level = level_max(level, Level::Error);
            details.push("ChatGPT endpoint=invalid or unsupported".to_string());
        }
    }

    if root.contains_key("profile") || root.contains_key("profiles") {
        level = level_max(level, Level::Warn);
        details.push(
            "legacy native profile settings detected; current Codex uses separate <name>.config.toml files"
                .to_string(),
        );
    }

    details.push(CONFIG_SCOPE_CAVEAT.to_string());
    Check::new(level, "codex_config", details.join("; "))
}

struct StorageState {
    level: Level,
    detail: String,
}

fn inspect_config_auth_storage(root: &toml::Table) -> StorageState {
    let value = root
        .get("cli_auth_credentials_store")
        .or_else(|| root.get("cli_auth_credentials_store_mode"));
    let Some(value) = value else {
        return StorageState {
            level: Level::Ok,
            detail: "auth storage=file (default; compatible)".to_string(),
        };
    };

    let Some(mode) = value.as_str() else {
        return StorageState {
            level: Level::Error,
            detail: "auth storage setting has invalid type".to_string(),
        };
    };

    match mode {
        "file" => StorageState {
            level: Level::Ok,
            detail: "auth storage=file (compatible)".to_string(),
        },
        "keyring" => StorageState {
            level: Level::Warn,
            detail: "auth storage=keyring (unsupported; Codex may keep credentials in the OS keyring, so auth.json is not authoritative)".to_string(),
        },
        "auto" => StorageState {
            level: Level::Warn,
            detail: "auth storage=auto (unsupported; Codex may use the OS keyring or encrypted secrets store, so auth.json is not authoritative)".to_string(),
        },
        "ephemeral" => StorageState {
            level: Level::Warn,
            detail: "auth storage=ephemeral (unsupported; credentials are process-local and cannot be switched from saved files)".to_string(),
        },
        _ => StorageState {
            level: Level::Error,
            detail: "auth storage setting is invalid (supported Codex values are file, keyring, auto, or ephemeral)".to_string(),
        },
    }
}

#[derive(Clone, Copy)]
enum EndpointState {
    OfficialDefault,
    Official,
    Loopback,
    Unrecognized,
    Invalid,
}

fn inspect_config_endpoint(paths: &Paths, root: &toml::Table) -> EndpointState {
    let Some(value) = root.get("chatgpt_base_url") else {
        return EndpointState::OfficialDefault;
    };
    let Some(value) = value.as_str() else {
        return EndpointState::Invalid;
    };

    // Use the same validation and normalization as usage requests.  The
    // returned URL is classified without ever including it in diagnostics.
    let accepted = crate::read_base_url(paths).is_ok();
    let shape = classify_endpoint_shape(value);
    match (accepted, shape) {
        (_, EndpointState::Invalid) => EndpointState::Invalid,
        (true, EndpointState::Official) => EndpointState::Official,
        (true, EndpointState::Loopback) => EndpointState::Loopback,
        (false, EndpointState::Official | EndpointState::Loopback) => EndpointState::Invalid,
        _ => EndpointState::Unrecognized,
    }
}

fn classify_endpoint_shape(value: &str) -> EndpointState {
    let Some((scheme, host)) = crate::parsed_url_scheme_and_host(value) else {
        return EndpointState::Invalid;
    };
    if scheme == "https" && matches!(host.as_str(), "chatgpt.com" | "chat.openai.com") {
        EndpointState::Official
    } else if matches!(scheme.as_str(), "http" | "https") && crate::is_loopback_host(&host) {
        EndpointState::Loopback
    } else {
        EndpointState::Unrecognized
    }
}

fn level_max(left: Level, right: Level) -> Level {
    fn rank(level: Level) -> u8 {
        match level {
            Level::Ok => 0,
            Level::Info => 1,
            Level::Warn => 2,
            Level::Error => 3,
        }
    }
    if rank(left) >= rank(right) {
        left
    } else {
        right
    }
}

fn summarize_checks(checks: &[Check]) -> Counts {
    let mut counts = Counts::default();
    for check in checks {
        counts.add(check.level);
    }
    counts
}

fn print_doctor_json(
    checks: Vec<Check>,
    summary: Counts,
    repairs: Option<Vec<String>>,
    error: Option<String>,
) -> Result<(), String> {
    let payload = DoctorJson {
        checks: checks
            .into_iter()
            .map(|check| DoctorCheckJson {
                name: check.name,
                level: check.level.label(),
                detail: check.detail,
            })
            .collect(),
        summary,
        repairs,
        error,
    };
    // All fields in DoctorJson are strings, counters, and vectors of those
    // values, so serde_json cannot fail for this schema.
    let json = serde_json::to_string_pretty(&payload)
        .expect("doctor JSON schema serialization is infallible");
    println!("{json}");
    Ok(())
}

fn inspect_install(_paths: &Paths) -> [Check; 2] {
    inspect_install_with_binary(std::env::current_exe())
}

fn inspect_install_with_binary(
    binary_result: Result<std::path::PathBuf, std::io::Error>,
) -> [Check; 2] {
    let binary = match binary_result {
        Ok(path) => Check::new(Level::Ok, "binary", path.display().to_string()),
        Err(err) => Check::new(Level::Error, "binary", err.to_string()),
    };
    let source = Check::new(
        Level::Info,
        "install source",
        install_source_label(detect_install_source()),
    );
    [binary, source]
}

fn inspect_auth(paths: &Paths, auth: &AuthState) -> Check {
    #[cfg(not(unix))]
    let _ = paths;
    match auth {
        AuthState::Missing => Check::new(Level::Warn, "auth file", "missing (run `codex login`)"),
        AuthState::Valid => {
            #[cfg(unix)]
            if let Ok(mode) = current_mode(&paths.auth)
                && mode != 0o600
            {
                return Check::new(
                    Level::Warn,
                    "auth file",
                    format!("valid (mode {mode:o}; run `doctor --fix`)"),
                );
            }
            Check::new(Level::Ok, "auth file", "valid")
        }
        AuthState::Incomplete(reason) => Check::new(Level::Warn, "auth file", reason),
        AuthState::Invalid(reason) => Check::new(
            Level::Error,
            "auth file",
            format!("{} (run `codex login`)", safe_auth_error(reason)),
        ),
    }
}

fn inspect_profiles_dir(paths: &Paths) -> Check {
    match fs::metadata(&paths.profiles) {
        Ok(meta) if meta.is_dir() => {
            #[cfg(unix)]
            if let Ok(mode) = current_mode(&paths.profiles)
                && mode != 0o700
            {
                return Check::new(
                    Level::Warn,
                    "profiles directory",
                    format!(
                        "{} (mode {mode:o}; run `doctor --fix`)",
                        paths.profiles.display()
                    ),
                );
            }
            Check::new(
                Level::Ok,
                "profiles directory",
                paths.profiles.display().to_string(),
            )
        }
        Ok(_) => Check::new(
            Level::Error,
            "profiles directory",
            "exists but is not a directory",
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Check::new(Level::Info, "profiles directory", "missing")
        }
        Err(err) => Check::new(Level::Error, "profiles directory", err.to_string()),
    }
}

fn inspect_profiles_index(paths: &Paths) -> Check {
    if !paths.profiles_index.exists() {
        Check::new(Level::Info, "profiles index", "missing")
    } else {
        match profiles_index_len_read_only(&paths.profiles_index) {
            Ok(count) => {
                #[cfg(unix)]
                if let Ok(mode) = current_mode(&paths.profiles_index)
                    && mode != 0o600
                {
                    return Check::new(
                        Level::Warn,
                        "profiles index",
                        format!("{count} entries (mode {mode:o}; run `doctor --fix`)"),
                    );
                }
                Check::new(Level::Ok, "profiles index", format!("{} entries", count))
            }
            Err(err) => Check::new(
                Level::Error,
                "profiles index",
                format!("{err} (run `doctor --fix` or remove profiles.json)"),
            ),
        }
    }
}

fn inspect_profiles_lock(paths: &Paths) -> Check {
    if !paths.profiles.exists() {
        Check::new(Level::Info, "profiles lock", "not created yet")
    } else if !paths.profiles_lock.exists() {
        Check::new(Level::Info, "profiles lock", "missing")
    } else if !paths.profiles_lock.is_file() {
        Check::new(Level::Error, "profiles lock", "exists but is not a file")
    } else {
        match lock_usage(paths) {
            Ok(_lock) => {
                #[cfg(unix)]
                if let Ok(mode) = current_mode(&paths.profiles_lock)
                    && mode != 0o600
                {
                    return Check::new(
                        Level::Warn,
                        "profiles lock",
                        format!("mode {mode:o} (run `doctor --fix`)"),
                    );
                }
                Check::new(Level::Ok, "profiles lock", "acquired")
            }
            Err(err) => Check::new(Level::Error, "profiles lock", err),
        }
    }
}

fn inspect_saved_profiles(paths: &Paths) -> SavedProfilesReport {
    let mut tokens = BTreeMap::new();
    let mut valid = 0usize;
    let mut invalid_ids = Vec::new();

    match profile_files(&paths.profiles).map(|mut v| {
        v.sort();
        v
    }) {
        Ok(paths_list) => {
            for path in paths_list {
                let id = profile_id_from_path(&path).unwrap_or_else(|| path.display().to_string());
                match read_tokens(&path) {
                    Ok(profile_tokens) if is_profile_ready(&profile_tokens) => {
                        valid += 1;
                        tokens.insert(id, Ok(profile_tokens));
                    }
                    Ok(_) => {
                        invalid_ids.push(id.clone());
                        tokens.insert(id, Err("profile is incomplete".to_string()));
                    }
                    Err(err) => {
                        invalid_ids.push(id.clone());
                        tokens.insert(id, Err(err));
                    }
                }
            }
        }
        Err(err) => {
            return SavedProfilesReport {
                check: Check::new(Level::Error, "saved profiles", err),
                tokens,
            };
        }
    }

    let check = if invalid_ids.is_empty() {
        Check::new(
            Level::Ok,
            "saved profiles",
            format!("{} valid, 0 invalid", valid),
        )
    } else {
        Check::new(
            Level::Warn,
            "saved profiles",
            format!(
                "{} valid, {} invalid (remove or re-save invalid profiles)",
                valid,
                invalid_ids.len()
            ),
        )
    };
    SavedProfilesReport { check, tokens }
}

fn create_profiles_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|err| err.to_string())
}

fn create_profiles_lock(path: &Path) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn inspect_current_profile(
    paths: &Paths,
    auth: &AuthState,
    tokens: &BTreeMap<String, Result<crate::Tokens, String>>,
) -> Check {
    match auth {
        AuthState::Missing => Check::new(Level::Info, "active profile", "no auth file"),
        AuthState::Incomplete(reason) => Check::new(
            Level::Warn,
            "active profile",
            format!("unavailable ({reason})"),
        ),
        AuthState::Invalid(reason) => Check::new(
            Level::Warn,
            "active profile",
            format!("unavailable ({})", safe_auth_error(reason)),
        ),
        AuthState::Valid => match current_saved_id(paths, tokens) {
            Some(_) => Check::new(Level::Ok, "active profile", "saved"),
            None => Check::new(
                Level::Warn,
                "active profile",
                "not saved (run `codex-profiles save`)",
            ),
        },
    }
}

fn auth_state(paths: &Paths) -> AuthState {
    match read_tokens(&paths.auth) {
        Ok(tokens) => {
            if is_profile_ready(&tokens) {
                AuthState::Valid
            } else {
                AuthState::Incomplete("present but incomplete (run `codex login`)".to_string())
            }
        }
        Err(err) => {
            if !paths.auth.exists() {
                AuthState::Missing
            } else {
                AuthState::Invalid(err)
            }
        }
    }
}

fn safe_auth_error(reason: &str) -> &str {
    if reason.starts_with("Error: Codex auth store mode ") {
        "Error: Codex uses an unsupported credential-store mode; see the codex_config check"
    } else {
        reason
    }
}

fn install_source_label(source: InstallSource) -> &'static str {
    match source {
        InstallSource::Npm => "npm",
        InstallSource::Bun => "bun",
        InstallSource::Brew => "brew",
        InstallSource::Unknown => "unknown",
    }
}

#[cfg(unix)]
fn repair_storage_permissions(paths: &Paths) -> Result<Vec<String>, String> {
    let mut repairs = Vec::new();

    if paths.auth.exists() && set_mode_if_needed(&paths.auth, 0o600)? {
        repairs.push("Repaired auth file permissions".to_string());
    }

    if set_mode_if_needed(&paths.profiles, 0o700)? {
        repairs.push("Repaired profiles directory permissions".to_string());
    }

    let mut repaired_metadata = 0usize;
    for path in [&paths.profiles_index, &paths.profiles_lock] {
        if path.exists() && set_mode_if_needed(path, 0o600)? {
            repaired_metadata += 1;
        }
    }
    if repaired_metadata > 0 {
        repairs.push(format!(
            "Repaired profile metadata permissions ({repaired_metadata})"
        ));
    }

    let mut repaired_profiles = 0usize;
    for path in profile_files(&paths.profiles)? {
        if set_mode_if_needed(&path, 0o600)? {
            repaired_profiles += 1;
        }
    }
    if repaired_profiles > 0 {
        repairs.push(format!(
            "Repaired saved profile permissions ({repaired_profiles})"
        ));
    }

    Ok(repairs)
}

#[cfg(not(unix))]
fn repair_storage_permissions(_paths: &Paths) -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

#[cfg(unix)]
fn set_mode_if_needed(path: &Path, mode: u32) -> Result<bool, String> {
    let current_mode = current_mode(path)?;
    if current_mode == mode {
        return Ok(false);
    }
    set_path_mode(path, mode)?;
    Ok(true)
}

#[cfg(unix)]
fn set_path_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|err| err.to_string())
}

#[cfg(unix)]
fn current_mode(path: &Path) -> Result<u32, String> {
    use std::os::unix::fs::PermissionsExt;

    Ok(fs::metadata(path)
        .map_err(|err| err.to_string())?
        .permissions()
        .mode()
        & 0o777)
}

fn profiles_index_len_read_only(path: &Path) -> Result<usize, String> {
    let raw = fs::read_to_string(path).map_err(|err| err.to_string())?;
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|err| err.to_string())?;
    let count = value
        .get("profiles")
        .and_then(|profiles| {
            profiles
                .as_object()
                .map(|entries| entries.len())
                .or_else(|| profiles.as_array().map(|entries| entries.len()))
        })
        .unwrap_or(0);
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::make_paths;
    use fslock::LockFile;
    use std::io;

    #[test]
    fn helper_classifiers_cover_error_and_info_states() {
        let checks = inspect_install_with_binary(Err(io::Error::other("no executable")));
        assert!(matches!(checks[0].level, Level::Error));
        assert_eq!(install_source_label(InstallSource::Npm), "npm");
        assert_eq!(install_source_label(InstallSource::Bun), "bun");
        assert_eq!(install_source_label(InstallSource::Brew), "brew");
        assert_eq!(install_source_label(InstallSource::Unknown), "unknown");
        assert_eq!(
            safe_auth_error("ordinary diagnostic"),
            "ordinary diagnostic"
        );
        assert_eq!(
            safe_auth_error("Error: Codex auth store mode keyring is unsupported"),
            "Error: Codex uses an unsupported credential-store mode; see the codex_config check"
        );
        assert!(matches!(level_max(Level::Info, Level::Ok), Level::Info));
        assert!(matches!(level_max(Level::Warn, Level::Info), Level::Warn));
        assert!(matches!(level_max(Level::Error, Level::Warn), Level::Error));
    }

    #[test]
    fn repair_and_config_inspection_cover_unreadable_and_invalid_states() {
        let dir = tempfile::tempdir().unwrap();
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        fs::write(&paths.profiles_index, b"not a directory").unwrap();
        fs::remove_file(&paths.profiles_index).unwrap();
        fs::create_dir(&paths.profiles_index).unwrap();
        assert_eq!(
            repair_storage(&paths).unwrap_err(),
            "Error: profiles index exists but is not a file"
        );

        fs::remove_dir(&paths.profiles_index).unwrap();
        fs::create_dir(&paths.profiles_lock).unwrap();
        assert_eq!(
            repair_storage(&paths).unwrap_err(),
            "Error: profiles lock exists but is not a file"
        );

        let dir = tempfile::tempdir().unwrap();
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.codex).unwrap();
        fs::create_dir(paths.codex.join("config.toml")).unwrap();
        let check = inspect_codex_config(&paths);
        assert!(check.detail.contains("unreadable"));

        let value: toml::Table = toml::from_str("cli_auth_credentials_store = 7").unwrap();
        assert!(matches!(
            inspect_config_auth_storage(&value).level,
            Level::Error
        ));
        let value: toml::Table = toml::from_str("").unwrap();
        assert!(matches!(
            inspect_config_auth_storage(&value).level,
            Level::Ok
        ));
        let value: toml::Table =
            toml::from_str("cli_auth_credentials_store = \"unknown\"").unwrap();
        assert!(matches!(
            inspect_config_auth_storage(&value).level,
            Level::Error
        ));
        let value: toml::Table = toml::from_str("chatgpt_base_url = 7").unwrap();
        assert!(matches!(
            inspect_config_endpoint(&paths, &value),
            EndpointState::Invalid
        ));

        let bad_parent = dir.path().join("not-a-directory");
        fs::write(&bad_parent, b"file").unwrap();
        assert!(create_profiles_directory(&bad_parent.join("profiles")).is_err());
        assert!(create_profiles_lock(&bad_parent.join("profiles.lock")).is_err());
        #[cfg(unix)]
        assert!(set_path_mode(Path::new("\0"), 0o600).is_err());
        #[cfg(unix)]
        assert!(set_mode_if_needed(Path::new("\0"), 0o600).is_err());
    }

    #[test]
    fn profile_checks_cover_invalid_paths_and_held_lock() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = make_paths(dir.path());
        paths.profiles = std::path::PathBuf::from("\0");
        assert!(matches!(inspect_profiles_dir(&paths).level, Level::Error));

        let dir = tempfile::tempdir().unwrap();
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        fs::create_dir(&paths.profiles_lock).unwrap();
        let check = inspect_profiles_lock(&paths);
        assert!(matches!(check.level, Level::Error));
        fs::remove_dir(&paths.profiles_lock).unwrap();
        fs::write(&paths.profiles_lock, b"").unwrap();
        let mut held = LockFile::open(&paths.profiles_lock).unwrap();
        held.lock().unwrap();
        let check = inspect_profiles_lock(&paths);
        assert!(matches!(check.level, Level::Error));
    }

    // Linux permits arbitrary byte sequences in directory entries; macOS
    // rejects this fixture before the doctor code can inspect it.
    #[cfg(target_os = "linux")]
    #[test]
    fn saved_profiles_use_a_safe_fallback_for_non_utf8_names() {
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        let name = std::ffi::OsString::from_vec(vec![0xff, b'.', b'j', b's', b'o', b'n']);
        fs::write(paths.profiles.join(name), b"not JSON").unwrap();
        let report = inspect_saved_profiles(&paths);
        assert!(matches!(report.check.level, Level::Warn));
    }

    #[test]
    fn doctor_fix_reports_json_error_and_plain_error() {
        let dir = tempfile::tempdir().unwrap();
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.codex).unwrap();
        fs::write(&paths.profiles, b"not a directory").unwrap();
        assert!(doctor(&paths, true, false).is_err());
        assert!(doctor(&paths, true, true).is_ok());
    }

    #[test]
    fn endpoint_and_storage_compatibility_cover_remaining_modes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.codex).unwrap();
        fs::write(
            paths.codex.join("config.toml"),
            "chatgpt_base_url = \"https://unrecognized.example\"\n",
        )
        .unwrap();
        let official: toml::Table =
            toml::from_str("chatgpt_base_url = \"https://chatgpt.com\"").unwrap();
        assert!(matches!(
            inspect_config_endpoint(&paths, &official),
            EndpointState::Invalid
        ));
        for mode in ["keyring", "auto", "ephemeral"] {
            let root: toml::Table =
                toml::from_str(&format!("cli_auth_credentials_store = \"{mode}\"")).unwrap();
            assert!(matches!(
                inspect_config_auth_storage(&root).level,
                Level::Warn
            ));
        }

        assert!(profiles_index_len_read_only(Path::new("\0")).is_err());
        let object = dir.path().join("object.json");
        fs::write(&object, r#"{"profiles":{"one":{},"two":{}}}"#).unwrap();
        assert_eq!(profiles_index_len_read_only(&object).unwrap(), 2);
        fs::write(&object, r#"{"profiles":[{},{}]}"#).unwrap();
        assert_eq!(profiles_index_len_read_only(&object).unwrap(), 2);
        fs::write(&object, r#"{"other":true}"#).unwrap();
        assert_eq!(profiles_index_len_read_only(&object).unwrap(), 0);
    }

    #[test]
    fn normal_path_checks_cover_existing_storage_and_active_profile() {
        let dir = tempfile::tempdir().unwrap();
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        fs::write(
            paths.codex.join("config.toml"),
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"https://chatgpt.com/backend-api\"\n",
        )
        .unwrap();
        let auth = br#"{"OPENAI_API_KEY":"sk-doctor-unit-test"}"#;
        fs::write(&paths.auth, auth).unwrap();
        fs::write(paths.profiles.join("saved.json"), auth).unwrap();
        fs::write(
            &paths.profiles_index,
            r#"{"version":3,"profiles":{"saved":{}}}"#,
        )
        .unwrap();
        fs::write(&paths.profiles_lock, b"").unwrap();

        let checks = collect_checks(&paths);
        assert!(checks.iter().any(|check| {
            check.name == "codex_config"
                && check.detail.contains("auth storage=file (compatible)")
                && check.detail.contains("official host")
        }));
        assert!(checks.iter().any(|check| {
            check.name == "profiles directory" && check.detail.contains("profiles")
        }));
        assert!(
            checks.iter().any(|check| {
                check.name == "profiles index" && check.detail.contains("1 entries")
            })
        );
        assert!(checks.iter().any(|check| {
            check.name == "profiles lock"
                && (check.detail == "acquired" || check.detail.contains("mode"))
        }));
        assert!(checks.iter().any(|check| {
            check.name == "saved profiles" && check.detail == "1 valid, 0 invalid"
        }));
        assert!(
            checks
                .iter()
                .any(|check| { check.name == "active profile" && check.detail == "saved" })
        );

        let repairs = repair(&paths).unwrap();
        assert!(!repairs.is_empty(), "expected permission repairs");
        assert!(repair(&paths).unwrap().is_empty());

        doctor(&paths, false, false).unwrap();
        doctor(&paths, false, true).unwrap();
        doctor(&paths, true, false).unwrap();
        doctor(&paths, true, true).unwrap();

        let fresh_dir = tempfile::tempdir().unwrap();
        let fresh = make_paths(fresh_dir.path());
        fs::create_dir_all(&fresh.codex).unwrap();
        // Exercise the human-readable repair report before storage exists.
        doctor(&fresh, true, false).unwrap();

        let storage_dir = tempfile::tempdir().unwrap();
        let storage_paths = make_paths(storage_dir.path());
        fs::create_dir_all(&storage_paths.codex).unwrap();
        let repairs = repair_storage(&storage_paths).unwrap();
        assert!(repairs.len() >= 2, "repairs: {repairs:?}");

        let missing_dir = tempfile::tempdir().unwrap();
        let missing = make_paths(missing_dir.path());
        assert!(matches!(inspect_profiles_dir(&missing).level, Level::Info));
        assert!(matches!(inspect_profiles_lock(&missing).level, Level::Info));
        fs::create_dir_all(&missing.profiles).unwrap();
        assert!(matches!(inspect_profiles_lock(&missing).level, Level::Info));

        for (config, expected) in [
            ("", "official default"),
            (
                "chatgpt_base_url = \"http://localhost:43123\"\n",
                "loopback host",
            ),
            (
                "chatgpt_base_url = \"https://unrecognized.example\"\n",
                "unrecognized host",
            ),
            (
                "chatgpt_base_url = \"http://[\"\n",
                "invalid or unsupported",
            ),
        ] {
            fs::write(fresh.codex.join("config.toml"), config).unwrap();
            let check = inspect_codex_config(&fresh);
            assert!(
                check.detail.contains(expected),
                "{config:?}: {}",
                check.detail
            );
        }
        fs::write(
            fresh.codex.join("config.toml"),
            "profile = \"legacy\"\n[profiles.work]\nmodel = \"private\"\n",
        )
        .unwrap();
        let check = inspect_codex_config(&fresh);
        assert!(
            check
                .detail
                .contains("legacy native profile settings detected")
        );
        fs::write(fresh.codex.join("config.toml"), "invalid = [\n").unwrap();
        let check = inspect_codex_config(&fresh);
        assert!(check.detail.contains("invalid TOML"));
    }
}
