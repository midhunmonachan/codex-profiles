use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::path::Path;

use serde::Serialize;

use crate::{
    InstallSource, Paths, current_saved_id, detect_install_source, is_profile_ready, lock_usage,
    print_output_block, profile_files, profile_id_from_path, read_tokens, repair_profiles_metadata,
};

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
        fs::create_dir_all(&paths.profiles).map_err(|err| err.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&paths.profiles, fs::Permissions::from_mode(0o700))
                .map_err(|err| err.to_string())?;
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
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&paths.profiles_lock)
            .map_err(|err| err.to_string())?;
        repairs.push("Created profiles lock file".to_string());
    }

    repairs.extend(repair_storage_permissions(paths)?);

    Ok(repairs)
}

fn collect_checks(paths: &Paths) -> Vec<Check> {
    let auth = auth_state(paths);
    let mut checks: Vec<Check> = inspect_install(paths).into_iter().collect();
    checks.push(inspect_auth(paths, &auth));
    checks.push(inspect_profiles_dir(paths));
    checks.push(inspect_profiles_index(paths));
    checks.push(inspect_profiles_lock(paths));

    let saved = inspect_saved_profiles(paths);
    checks.push(saved.check);
    checks.push(inspect_current_profile(paths, &auth, &saved.tokens));
    checks
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
    let json = serde_json::to_string_pretty(&payload).map_err(|err| err.to_string())?;
    println!("{json}");
    Ok(())
}

fn inspect_install(_paths: &Paths) -> [Check; 2] {
    let binary = match std::env::current_exe() {
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
            format!("{reason} (run `codex login`)"),
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

fn inspect_current_profile(
    paths: &Paths,
    auth: &AuthState,
    tokens: &BTreeMap<String, Result<crate::Tokens, String>>,
) -> Check {
    match auth {
        AuthState::Missing => Check::new(Level::Info, "active profile", "no auth file"),
        AuthState::Incomplete(reason) | AuthState::Invalid(reason) => Check::new(
            Level::Warn,
            "active profile",
            format!("unavailable ({reason})"),
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
    use std::os::unix::fs::PermissionsExt;

    let current_mode = current_mode(path)?;
    if current_mode == mode {
        return Ok(false);
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|err| err.to_string())?;
    Ok(true)
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
