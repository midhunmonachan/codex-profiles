use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::IsTerminal as _;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration as StdDuration;

use crate::{Paths, lock_usage, read_profiles_index, write_atomic, write_profiles_index};
use crate::{
    UPDATE_ERR_PERSIST_DISMISSAL, UPDATE_ERR_READ_CHOICE, UPDATE_ERR_REFRESH_VERSION,
    UPDATE_ERR_SHOW_PROMPT, UPDATE_NON_TTY_RUN, UPDATE_OPTION_NOW, UPDATE_OPTION_SKIP,
    UPDATE_OPTION_SKIP_VERSION, UPDATE_PROMPT_SELECT, UPDATE_RELEASE_NOTES, UPDATE_TITLE_AVAILABLE,
};

// We use the latest version from the cask if installation is via homebrew - homebrew does not immediately pick up the latest release and can lag behind.
const HOMEBREW_CASK_URL: &str =
    "https://raw.githubusercontent.com/Homebrew/homebrew-cask/HEAD/Casks/c/codex-profiles.rb";
const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/midhunmonachan/codex-profiles/releases/latest";
const RELEASE_NOTES_URL: &str = "https://github.com/midhunmonachan/codex-profiles/releases/latest";
#[cfg(test)]
const HOMEBREW_CASK_URL_OVERRIDE_ENV_VAR: &str = "CODEX_PROFILES_HOMEBREW_CASK_URL";
#[cfg(test)]
const LATEST_RELEASE_URL_OVERRIDE_ENV_VAR: &str = "CODEX_PROFILES_LATEST_RELEASE_URL";

/// Update action the CLI should perform after the prompt exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    /// Update via `npm install -g codex-profiles`.
    NpmGlobalLatest,
    /// Update via `bun install -g codex-profiles`.
    BunGlobalLatest,
    /// Update via `brew upgrade codex-profiles`.
    BrewUpgrade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallSource {
    Npm,
    Bun,
    Brew,
    Unknown,
}

impl UpdateAction {
    /// Returns the list of command-line arguments for invoking the update.
    pub fn command_args(self) -> (&'static str, &'static [&'static str]) {
        match self {
            UpdateAction::NpmGlobalLatest => ("npm", &["install", "-g", "codex-profiles"]),
            UpdateAction::BunGlobalLatest => ("bun", &["install", "-g", "codex-profiles"]),
            UpdateAction::BrewUpgrade => ("brew", &["upgrade", "codex-profiles"]),
        }
    }

    /// Returns string representation of the command-line arguments for invoking the update.
    pub fn command_str(self) -> String {
        let (command, args) = self.command_args();
        std::iter::once(command)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub fn detect_install_source() -> InstallSource {
    let exe = std::env::current_exe().unwrap_or_default();
    let managed_by_npm = std::env::var_os("CODEX_PROFILES_MANAGED_BY_NPM").is_some();
    let managed_by_bun = std::env::var_os("CODEX_PROFILES_MANAGED_BY_BUN").is_some();
    detect_install_source_inner(
        cfg!(target_os = "macos"),
        &exe,
        managed_by_npm,
        managed_by_bun,
    )
}

#[doc(hidden)]
pub fn detect_install_source_inner(
    is_macos: bool,
    current_exe: &std::path::Path,
    managed_by_npm: bool,
    managed_by_bun: bool,
) -> InstallSource {
    let bun_install = is_bun_install(current_exe);
    if managed_by_npm {
        InstallSource::Npm
    } else if managed_by_bun && bun_install {
        InstallSource::Bun
    } else if is_macos && is_brew_install(current_exe) {
        InstallSource::Brew
    } else if bun_install {
        InstallSource::Bun
    } else if managed_by_bun {
        // Defensive fallback: older wrappers may leak bun hints from ambient env
        // even when the installed binary is not managed by bun.
        InstallSource::Npm
    } else {
        InstallSource::Unknown
    }
}

fn is_bun_install(current_exe: &std::path::Path) -> bool {
    let exe = current_exe.to_string_lossy();
    exe.contains(".bun/install/global") || exe.contains(".bun\\install\\global")
}

fn is_brew_install(current_exe: &std::path::Path) -> bool {
    (current_exe.starts_with("/opt/homebrew") || current_exe.starts_with("/usr/local"))
        && current_exe.file_name().and_then(|name| name.to_str()) == Some("codex-profiles")
}

pub(crate) fn get_update_action() -> Option<UpdateAction> {
    get_update_action_with_debug(cfg!(debug_assertions), detect_install_source())
}

fn get_update_action_with_debug(
    is_debug: bool,
    install_source: InstallSource,
) -> Option<UpdateAction> {
    if is_debug {
        return None;
    }
    match install_source {
        InstallSource::Npm => Some(UpdateAction::NpmGlobalLatest),
        InstallSource::Bun => Some(UpdateAction::BunGlobalLatest),
        InstallSource::Brew => Some(UpdateAction::BrewUpgrade),
        InstallSource::Unknown => None,
    }
}

#[derive(Clone, Debug)]
pub struct UpdateConfig {
    pub codex_home: PathBuf,
    pub check_for_update_on_startup: bool,
}

#[derive(Deserialize, Debug, Clone)]
struct ReleaseInfo {
    tag_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateCache {
    #[serde(default)]
    latest_version: String,
    #[serde(default = "update_cache_checked_default")]
    last_checked_at: DateTime<Utc>,
    #[serde(default)]
    dismissed_version: Option<String>,
    #[serde(default)]
    last_prompted_at: Option<DateTime<Utc>>,
}

fn update_cache_checked_default() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now)
}

#[derive(Debug, PartialEq, Eq)]
pub enum UpdatePromptOutcome {
    Continue,
    RunUpdate(UpdateAction),
}

pub fn run_update_prompt_if_needed(config: &UpdateConfig) -> Result<UpdatePromptOutcome, String> {
    let mut input = io::stdin().lock();
    let mut output = io::stderr();
    run_update_prompt_if_needed_with_io(
        config,
        cfg!(debug_assertions),
        io::stdin().is_terminal(),
        &mut input,
        &mut output,
    )
}

fn run_update_prompt_if_needed_with_io(
    config: &UpdateConfig,
    is_debug: bool,
    is_tty: bool,
    input: &mut dyn io::BufRead,
    output: &mut dyn Write,
) -> Result<UpdatePromptOutcome, String> {
    run_update_prompt_if_needed_with_io_and_source(
        config,
        is_debug,
        is_tty,
        detect_install_source(),
        input,
        output,
    )
}

fn run_update_prompt_if_needed_with_io_and_source(
    config: &UpdateConfig,
    is_debug: bool,
    is_tty: bool,
    install_source: InstallSource,
    input: &mut dyn io::BufRead,
    output: &mut dyn Write,
) -> Result<UpdatePromptOutcome, String> {
    if is_debug {
        return Ok(UpdatePromptOutcome::Continue);
    }

    let Some(latest_version) = get_upgrade_version_for_popup_with_debug(config, is_debug) else {
        return Ok(UpdatePromptOutcome::Continue);
    };
    let Some(update_action) = get_update_action_with_debug(false, install_source) else {
        return Ok(UpdatePromptOutcome::Continue);
    };

    let current_version = current_version();
    if !is_tty {
        write_prompt(
            output,
            format_args!(
                "{} {current_version} -> {latest_version}\n",
                UPDATE_TITLE_AVAILABLE
            ),
        )?;
        write_prompt(
            output,
            format_args!(
                "{}",
                crate::msg1(UPDATE_NON_TTY_RUN, update_action.command_str())
            ),
        )?;
        return Ok(UpdatePromptOutcome::Continue);
    }

    let prompt = format!(
        "\n✨ {} {current_version} -> {latest_version}\n{}\n{}{}{}{}",
        UPDATE_TITLE_AVAILABLE,
        crate::msg1(UPDATE_RELEASE_NOTES, RELEASE_NOTES_URL),
        crate::msg1(UPDATE_OPTION_NOW, update_action.command_str()),
        UPDATE_OPTION_SKIP,
        UPDATE_OPTION_SKIP_VERSION,
        UPDATE_PROMPT_SELECT,
    );
    write_prompt(output, format_args!("{prompt}"))?;
    output.flush().map_err(prompt_io_error)?;
    let _ = mark_prompted_now(config);

    let mut selection = String::new();
    input
        .read_line(&mut selection)
        .map_err(|err| crate::msg1(UPDATE_ERR_READ_CHOICE, err))?;

    match selection.trim() {
        "1" => Ok(UpdatePromptOutcome::RunUpdate(update_action)),
        "3" => {
            if let Err(err) = dismiss_version(config, &latest_version) {
                write_prompt(
                    output,
                    format_args!("{}", crate::msg1(UPDATE_ERR_PERSIST_DISMISSAL, err)),
                )?;
            }
            Ok(UpdatePromptOutcome::Continue)
        }
        _ => Ok(UpdatePromptOutcome::Continue),
    }
}

fn prompt_io_error(err: impl std::fmt::Display) -> String {
    crate::msg1(UPDATE_ERR_SHOW_PROMPT, err)
}

fn write_prompt(output: &mut dyn Write, args: std::fmt::Arguments) -> Result<(), String> {
    output.write_fmt(args).map_err(prompt_io_error)
}

fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn build_update_cache(
    latest_version: Option<String>,
    dismissed_version: Option<String>,
    last_prompted_at: Option<DateTime<Utc>>,
) -> UpdateCache {
    UpdateCache {
        latest_version: latest_version.unwrap_or_else(|| current_version().to_string()),
        last_checked_at: Utc::now(),
        dismissed_version,
        last_prompted_at,
    }
}

fn get_upgrade_version_with_debug(config: &UpdateConfig, is_debug: bool) -> Option<String> {
    if updates_disabled_with_debug(config, is_debug) {
        return None;
    }
    let paths = paths_for_update(config.codex_home.clone());
    let mut info = read_update_cache(&paths).ok().flatten();

    let should_check = match &info {
        None => true,
        Some(info) => info.last_checked_at < Utc::now() - Duration::hours(20),
    };
    if should_check {
        if info.is_none() {
            if let Err(err) = check_for_update(&paths) {
                eprintln!("{}", crate::msg1(UPDATE_ERR_REFRESH_VERSION, err));
            }
            info = read_update_cache(&paths).ok().flatten();
        } else {
            let codex_home = config.codex_home.clone();
            std::thread::spawn(move || {
                let paths = paths_for_update(codex_home);
                if let Err(err) = check_for_update(&paths) {
                    eprintln!("{}", crate::msg1(UPDATE_ERR_REFRESH_VERSION, err));
                }
            });
        }
    }

    info.and_then(|info| {
        if is_newer(&info.latest_version, current_version()).unwrap_or(false) {
            Some(info.latest_version)
        } else {
            None
        }
    })
}

fn check_for_update(paths: &Paths) -> Result<(), String> {
    check_for_update_with_action(paths, get_update_action())
}

fn check_for_update_with_action(
    paths: &Paths,
    update_action: Option<UpdateAction>,
) -> Result<(), String> {
    let latest_version = match update_action {
        Some(UpdateAction::BrewUpgrade) => {
            fetch_version_from_cask().or_else(fetch_version_from_release)
        }
        _ => fetch_version_from_release(),
    };

    // Preserve any previously dismissed version if present.
    let prev_info = read_update_cache(paths).ok().flatten();
    let prev_dismissed = prev_info
        .as_ref()
        .and_then(|info| info.dismissed_version.clone());
    let prev_prompted = prev_info.as_ref().and_then(|info| info.last_prompted_at);
    let info = build_update_cache(latest_version, prev_dismissed, prev_prompted);
    write_update_cache(paths, &info)
}

#[doc(hidden)]
pub fn is_newer(latest: &str, current: &str) -> Option<bool> {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => Some(l > c),
        _ => None,
    }
}

#[doc(hidden)]
pub fn extract_version_from_cask(cask_contents: &str) -> Result<String, String> {
    cask_contents
        .lines()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("version \"")
                .and_then(|rest| rest.strip_suffix('"'))
                .map(ToString::to_string)
        })
        .ok_or_else(|| "Failed to find version in Homebrew cask file".to_string())
}

#[doc(hidden)]
pub fn extract_version_from_latest_tag(latest_tag_name: &str) -> Result<String, String> {
    for prefix in ["v", "rust-v"] {
        if let Some(version) = latest_tag_name.strip_prefix(prefix) {
            return Ok(version.to_string());
        }
    }
    Err(format!(
        "Failed to parse latest tag name '{latest_tag_name}'"
    ))
}

fn fetch_version_from_cask() -> Option<String> {
    let response = update_agent()
        .get(&homebrew_cask_url())
        .header("User-Agent", "codex-profiles")
        .call();
    match response {
        Ok(mut resp) => {
            let contents = resp.body_mut().read_to_string().ok()?;
            extract_version_from_cask(&contents).ok()
        }
        Err(ureq::Error::StatusCode(404)) => None,
        Err(_) => None,
    }
}

fn fetch_version_from_release() -> Option<String> {
    let response = update_agent()
        .get(&latest_release_url())
        .header("User-Agent", "codex-profiles")
        .call();
    match response {
        Ok(mut resp) => {
            let ReleaseInfo {
                tag_name: latest_tag_name,
            } = resp.body_mut().read_json().ok()?;
            extract_version_from_latest_tag(&latest_tag_name).ok()
        }
        Err(ureq::Error::StatusCode(404)) => None,
        Err(_) => None,
    }
}

fn get_upgrade_version_for_popup_with_debug(
    config: &UpdateConfig,
    is_debug: bool,
) -> Option<String> {
    if updates_disabled_with_debug(config, is_debug) {
        return None;
    }

    let paths = paths_for_update(config.codex_home.clone());
    let latest = get_upgrade_version_with_debug(config, is_debug)?;
    let info = read_update_cache(&paths).ok().flatten();
    if info
        .as_ref()
        .and_then(|info| info.last_prompted_at)
        .is_some_and(|last| last > Utc::now() - Duration::hours(24))
    {
        return None;
    }
    // If the user dismissed this exact version previously, do not show the popup.
    if info
        .as_ref()
        .and_then(|info| info.dismissed_version.as_deref())
        == Some(latest.as_str())
    {
        return None;
    }
    Some(latest)
}

/// Persist a dismissal for the current latest version so we don't show
/// the update popup again for this version.
pub fn dismiss_version(config: &UpdateConfig, version: &str) -> Result<(), String> {
    if !config.check_for_update_on_startup {
        return Ok(());
    }
    let paths = paths_for_update(config.codex_home.clone());
    let mut info = match read_update_cache(&paths) {
        Ok(Some(info)) => info,
        _ => return Ok(()),
    };
    info.dismissed_version = Some(version.to_string());
    info.last_prompted_at = Some(Utc::now());
    write_update_cache(&paths, &info)
}

fn mark_prompted_now(config: &UpdateConfig) -> Result<(), String> {
    if !config.check_for_update_on_startup {
        return Ok(());
    }
    let paths = paths_for_update(config.codex_home.clone());
    let mut info = match read_update_cache(&paths) {
        Ok(Some(info)) => info,
        _ => return Ok(()),
    };
    info.last_prompted_at = Some(Utc::now());
    write_update_cache(&paths, &info)
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut iter = v.trim().split('.');
    let maj_raw = iter.next().unwrap_or_default();
    let min_raw = iter.next()?;
    let pat_raw = iter.next()?;
    let maj = parse_version_component(maj_raw);
    let min = parse_version_component(min_raw);
    let pat = parse_version_component(pat_raw);
    let (Some(maj), Some(min), Some(pat)) = (maj, min, pat) else {
        return None;
    };
    Some((maj, min, pat))
}

fn parse_version_component(value: &str) -> Option<u64> {
    value.parse::<u64>().ok()
}

fn updates_disabled_with_debug(config: &UpdateConfig, is_debug: bool) -> bool {
    is_debug || !config.check_for_update_on_startup
}

fn paths_for_update(codex_home: PathBuf) -> Paths {
    let profiles = codex_home.join("profiles");
    Paths {
        auth: codex_home.join("auth.json"),
        profiles_index: profiles.join("profiles.json"),
        update_cache: profiles.join("update.json"),
        profiles_lock: profiles.join("profiles.lock"),
        codex: codex_home,
        profiles,
    }
}

fn read_update_cache(paths: &Paths) -> Result<Option<UpdateCache>, String> {
    if !paths.update_cache.exists() {
        if let Some(legacy) = read_legacy_update_cache(paths)? {
            let _ = write_update_cache(paths, &legacy);
            return Ok(Some(legacy));
        }
        return Ok(None);
    }
    let contents = fs::read_to_string(&paths.update_cache).map_err(|e| e.to_string())?;
    if contents.trim().is_empty() {
        return Ok(None);
    }
    let cache = serde_json::from_str::<UpdateCache>(&contents).map_err(|e| e.to_string())?;
    Ok(Some(cache))
}

fn write_update_cache(paths: &Paths, cache: &UpdateCache) -> Result<(), String> {
    let _lock = lock_usage(paths)?;
    let contents = serde_json::to_string_pretty(cache)
        .expect("UpdateCache contains only infallible JSON values");
    write_atomic(&paths.update_cache, format!("{contents}\n").as_bytes())
}

fn read_legacy_update_cache(paths: &Paths) -> Result<Option<UpdateCache>, String> {
    if !paths.profiles_index.exists() {
        return Ok(None);
    }
    let contents = match fs::read_to_string(&paths.profiles_index) {
        Ok(contents) => contents,
        Err(err) => return Err(err.to_string()),
    };
    let json: serde_json::Value = serde_json::from_str(&contents).map_err(|e| e.to_string())?;
    let Some(value) = json.get("update_cache") else {
        return Ok(None);
    };
    let cache = serde_json::from_value::<UpdateCache>(value.clone()).map_err(|e| e.to_string())?;
    if let Ok(index) = read_profiles_index(paths) {
        let _ = write_profiles_index(paths, &index);
    }
    Ok(Some(cache))
}

fn update_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(StdDuration::from_secs(5)))
        .build();
    config.into()
}

fn latest_release_url() -> String {
    #[cfg(test)]
    if let Ok(url) = std::env::var(LATEST_RELEASE_URL_OVERRIDE_ENV_VAR) {
        return url;
    }
    LATEST_RELEASE_URL.to_string()
}

fn homebrew_cask_url() -> String {
    #[cfg(test)]
    if let Ok(url) = std::env::var(HOMEBREW_CASK_URL_OVERRIDE_ENV_VAR) {
        return url;
    }
    HOMEBREW_CASK_URL.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{ENV_MUTEX, http_ok_response, set_env_guard, spawn_server};
    use std::fs;
    use std::io::{self, Read};
    use std::path::PathBuf;

    fn seed_version_info(config: &UpdateConfig, version: &str) {
        let paths = paths_for_update(config.codex_home.clone());
        fs::create_dir_all(&paths.profiles).unwrap();
        fs::write(&paths.profiles_lock, "").unwrap();
        let info = UpdateCache {
            latest_version: version.to_string(),
            last_checked_at: Utc::now(),
            dismissed_version: None,
            last_prompted_at: None,
        };
        write_update_cache(&paths, &info).unwrap();
    }

    #[test]
    fn update_action_commands() {
        let (cmd, args) = UpdateAction::NpmGlobalLatest.command_args();
        assert_eq!(cmd, "npm");
        assert!(args.contains(&"install"));
        assert!(UpdateAction::BunGlobalLatest.command_str().contains("bun"));
        assert_eq!(
            UpdateAction::BrewUpgrade.command_args(),
            ("brew", &["upgrade", "codex-profiles"] as &[&str])
        );
    }

    #[test]
    fn detect_install_source_inner_variants() {
        let exe = PathBuf::from("/usr/local/bin/codex-profiles");
        assert_eq!(
            detect_install_source_inner(true, &exe, false, false),
            InstallSource::Brew
        );
        assert_eq!(
            detect_install_source_inner(false, &exe, true, false),
            InstallSource::Npm
        );
        assert_eq!(
            detect_install_source_inner(false, &exe, false, true),
            InstallSource::Npm
        );

        let bun_exe = PathBuf::from(
            "/Users/dev/.bun/install/global/node_modules/codex-profiles/bin/codex-profiles",
        );
        assert_eq!(
            detect_install_source_inner(false, &bun_exe, false, true),
            InstallSource::Bun
        );
        assert_eq!(
            detect_install_source_inner(false, &bun_exe, false, false),
            InstallSource::Bun
        );
    }

    #[test]
    fn get_update_action_debug() {
        assert!(get_update_action_with_debug(true, InstallSource::Npm).is_none());
        assert!(get_update_action_with_debug(false, InstallSource::Npm).is_some());
        assert_eq!(
            get_update_action_with_debug(false, InstallSource::Bun),
            Some(UpdateAction::BunGlobalLatest)
        );
        assert_eq!(
            get_update_action_with_debug(false, InstallSource::Brew),
            Some(UpdateAction::BrewUpgrade)
        );
        assert_eq!(
            get_update_action_with_debug(false, InstallSource::Unknown),
            None
        );
        let _ = get_update_action();
    }

    #[test]
    fn extract_version_helpers() {
        assert_eq!(extract_version_from_latest_tag("v1.2.3").unwrap(), "1.2.3");
        assert_eq!(
            extract_version_from_latest_tag("rust-v2.0.0").unwrap(),
            "2.0.0"
        );
        assert!(extract_version_from_latest_tag("bad").is_err());
        let cask = "version \"1.2.3\"";
        assert_eq!(extract_version_from_cask(cask).unwrap(), "1.2.3");
        assert!(extract_version_from_cask("nope").is_err());
    }

    #[test]
    fn parse_version_and_compare() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("bad.2.3"), None);
        assert_eq!(parse_version("1.bad.3"), None);
        assert_eq!(parse_version("1.2.bad"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1"), None);
        assert_eq!(parse_version(""), None);
        assert!(is_newer("2.0.0", "1.9.9").unwrap());
        assert!(is_newer("bad", "1.0.0").is_none());
    }

    #[test]
    fn url_overrides_work() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let _env = set_env_guard(
            LATEST_RELEASE_URL_OVERRIDE_ENV_VAR,
            Some("http://example.com"),
        );
        assert_eq!(latest_release_url(), "http://example.com");
        drop(_env);
        let _latest_default = set_env_guard(LATEST_RELEASE_URL_OVERRIDE_ENV_VAR, None);
        assert_eq!(latest_release_url(), LATEST_RELEASE_URL);
        let _cask_default = set_env_guard(HOMEBREW_CASK_URL_OVERRIDE_ENV_VAR, None);
        assert_eq!(homebrew_cask_url(), HOMEBREW_CASK_URL);
    }

    #[test]
    fn fetch_versions_from_servers() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let release_body = "{\"tag_name\":\"v9.9.9\"}";
        let release_resp = http_ok_response(release_body, "application/json");
        let release_url = spawn_server(release_resp);
        {
            let _env = set_env_guard(LATEST_RELEASE_URL_OVERRIDE_ENV_VAR, Some(&release_url));
            assert_eq!(fetch_version_from_release().unwrap(), "9.9.9");
        }

        let cask_body = "version \"9.9.9\"";
        let cask_resp = http_ok_response(cask_body, "text/plain");
        let cask_url = spawn_server(cask_resp);
        {
            let _env = set_env_guard(HOMEBREW_CASK_URL_OVERRIDE_ENV_VAR, Some(&cask_url));
            assert_eq!(fetch_version_from_cask().unwrap(), "9.9.9");
        }
    }

    #[test]
    fn fetch_versions_handle_404() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string();
        let url = spawn_server(resp);
        let _env = set_env_guard(LATEST_RELEASE_URL_OVERRIDE_ENV_VAR, Some(&url));
        assert!(fetch_version_from_release().is_none());
    }

    #[test]
    fn fetch_versions_handle_invalid_bodies() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let short_body = spawn_server(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\n\r\nx"
                .to_string(),
        );
        let _cask_env = set_env_guard(HOMEBREW_CASK_URL_OVERRIDE_ENV_VAR, Some(&short_body));
        assert!(fetch_version_from_cask().is_none());

        let malformed_json = spawn_server(http_ok_response("{", "application/json"));
        let _release_env =
            set_env_guard(LATEST_RELEASE_URL_OVERRIDE_ENV_VAR, Some(&malformed_json));
        assert!(fetch_version_from_release().is_none());
    }

    #[test]
    fn check_for_update_writes_version() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let release_body = "{\"tag_name\":\"v9.9.9\"}";
        let release_resp = http_ok_response(release_body, "application/json");
        let release_url = spawn_server(release_resp);
        let _env = set_env_guard(LATEST_RELEASE_URL_OVERRIDE_ENV_VAR, Some(&release_url));

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for_update(dir.path().to_path_buf());
        fs::create_dir_all(&paths.profiles).unwrap();
        fs::write(&paths.profiles_lock, "").unwrap();
        check_for_update_with_action(&paths, None).unwrap();
        let contents = fs::read_to_string(&paths.update_cache).unwrap();
        assert!(contents.contains("9.9.9"));
    }

    #[test]
    fn read_update_cache_migrates_legacy_profiles_schema() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for_update(dir.path().to_path_buf());
        fs::create_dir_all(&paths.profiles).unwrap();
        fs::write(&paths.profiles_lock, "").unwrap();
        let legacy = serde_json::json!({
            "version": 1,
            "profiles": {},
            "update_cache": {
                "latest_version": "1.2.3",
                "last_checked_at": "2024-01-01T00:00:00Z"
            }
        });
        fs::write(
            &paths.profiles_index,
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let migrated = read_update_cache(&paths).unwrap().unwrap();
        assert_eq!(migrated.latest_version, "1.2.3");
        assert!(paths.update_cache.is_file());
        let index_contents = fs::read_to_string(&paths.profiles_index).unwrap();
        assert!(!index_contents.contains("update_cache"));
    }

    #[test]
    fn updates_disabled_variants() {
        let config = UpdateConfig {
            codex_home: PathBuf::new(),
            check_for_update_on_startup: false,
        };
        assert!(updates_disabled_with_debug(&config, false));
        assert!(get_upgrade_version_with_debug(&config, false).is_none());
        let config = UpdateConfig {
            codex_home: PathBuf::new(),
            check_for_update_on_startup: true,
        };
        assert!(updates_disabled_with_debug(&config, true));
        assert!(get_upgrade_version_with_debug(&config, false).is_none());
        assert!(get_upgrade_version_for_popup_with_debug(&config, false).is_none());
        let disabled = UpdateConfig {
            codex_home: PathBuf::new(),
            check_for_update_on_startup: false,
        };
        assert!(get_upgrade_version_for_popup_with_debug(&disabled, false).is_none());
    }

    #[test]
    fn update_prompt_debug_mode_is_a_noop() {
        let config = UpdateConfig {
            codex_home: PathBuf::new(),
            check_for_update_on_startup: true,
        };
        assert_eq!(
            run_update_prompt_if_needed(&config).unwrap(),
            UpdatePromptOutcome::Continue
        );

        let mut input = std::io::Cursor::new("");
        let mut output = Vec::new();
        assert_eq!(
            run_update_prompt_if_needed_with_io_and_source(
                &config,
                true,
                true,
                InstallSource::Npm,
                &mut input,
                &mut output,
            )
            .unwrap(),
            UpdatePromptOutcome::Continue
        );
        assert!(output.is_empty());
    }

    #[test]
    fn run_update_prompt_paths() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let release_body = format!("{{\"tag_name\":\"v{}\"}}", "99.0.0");
        let release_resp = http_ok_response(&release_body, "application/json");
        let release_url = spawn_server(release_resp);
        let _env = set_env_guard(LATEST_RELEASE_URL_OVERRIDE_ENV_VAR, Some(&release_url));

        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::Cursor::new("2\n");
        let mut output = Vec::new();
        let result = run_update_prompt_if_needed_with_io_and_source(
            &config,
            false,
            false,
            InstallSource::Npm,
            &mut input,
            &mut output,
        )
        .unwrap();
        assert_eq!(result, UpdatePromptOutcome::Continue);

        let cache = read_update_cache(&paths_for_update(config.codex_home.clone()))
            .unwrap()
            .expect("update cache");
        assert!(cache.last_prompted_at.is_none());
        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("Run `npm install -g codex-profiles` to update."));

        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::Cursor::new("");
        let mut output = Vec::new();
        let result = run_update_prompt_if_needed_with_io_and_source(
            &config,
            false,
            false,
            InstallSource::Unknown,
            &mut input,
            &mut output,
        )
        .unwrap();
        assert_eq!(result, UpdatePromptOutcome::Continue);

        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::Cursor::new("1\n");
        let mut output = Vec::new();
        let result = run_update_prompt_if_needed_with_io_and_source(
            &config,
            false,
            true,
            InstallSource::Npm,
            &mut input,
            &mut output,
        )
        .unwrap();
        assert_eq!(
            result,
            UpdatePromptOutcome::RunUpdate(UpdateAction::NpmGlobalLatest)
        );

        let cache = read_update_cache(&paths_for_update(config.codex_home.clone()))
            .unwrap()
            .expect("update cache");
        assert!(cache.last_prompted_at.is_some());

        let mut input = std::io::Cursor::new("1\n");
        let mut output = Vec::new();
        let result = run_update_prompt_if_needed_with_io_and_source(
            &config,
            false,
            true,
            InstallSource::Npm,
            &mut input,
            &mut output,
        )
        .unwrap();
        assert_eq!(result, UpdatePromptOutcome::Continue);
        assert!(output.is_empty());

        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::Cursor::new("3\n");
        let mut output = Vec::new();
        let result = run_update_prompt_if_needed_with_io_and_source(
            &config,
            false,
            true,
            InstallSource::Npm,
            &mut input,
            &mut output,
        )
        .unwrap();
        assert_eq!(result, UpdatePromptOutcome::Continue);

        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::Cursor::new("2\n");
        let mut output = Vec::new();
        let result = run_update_prompt_if_needed_with_io_and_source(
            &config,
            false,
            true,
            InstallSource::Npm,
            &mut input,
            &mut output,
        )
        .unwrap();
        assert_eq!(result, UpdatePromptOutcome::Continue);
    }

    struct FailingWriter {
        writes: usize,
        fail_write: Option<usize>,
        fail_contains: Option<&'static [u8]>,
        fail_flush: bool,
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("synthetic read failure"))
        }
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if self.fail_write == Some(self.writes)
                || self
                    .fail_contains
                    .is_some_and(|needle| bytes.windows(needle.len()).any(|part| part == needle))
            {
                return Err(io::Error::other("synthetic write failure"));
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                Err(io::Error::other("synthetic flush failure"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn prompt_reports_output_errors_and_unrecognised_choices() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::Cursor::new("");
        let mut output = FailingWriter {
            writes: 0,
            fail_write: None,
            fail_contains: Some(b"Run `"),
            fail_flush: false,
        };
        let result = run_update_prompt_if_needed_with_io_and_source(
            &config,
            false,
            false,
            InstallSource::Npm,
            &mut input,
            &mut output,
        );
        assert!(result.is_err(), "writes: {}", output.writes);

        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::Cursor::new("");
        let mut output = FailingWriter {
            writes: 0,
            fail_write: Some(1),
            fail_contains: None,
            fail_flush: false,
        };
        assert!(
            run_update_prompt_if_needed_with_io_and_source(
                &config,
                false,
                false,
                InstallSource::Npm,
                &mut input,
                &mut output,
            )
            .is_err()
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::Cursor::new("2\n");
        let mut output = FailingWriter {
            writes: 0,
            fail_write: Some(1),
            fail_contains: None,
            fail_flush: false,
        };
        let result = run_update_prompt_if_needed_with_io_and_source(
            &config,
            false,
            true,
            InstallSource::Npm,
            &mut input,
            &mut output,
        );
        assert!(result.is_err(), "writes: {}", output.writes);

        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::Cursor::new("2\n");
        let mut output = FailingWriter {
            writes: 0,
            fail_write: None,
            fail_contains: Some(b"2) Skip"),
            fail_flush: false,
        };
        assert!(
            run_update_prompt_if_needed_with_io_and_source(
                &config,
                false,
                true,
                InstallSource::Npm,
                &mut input,
                &mut output,
            )
            .is_err()
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::Cursor::new("2\n");
        let mut output = FailingWriter {
            writes: 0,
            fail_write: None,
            fail_contains: None,
            fail_flush: true,
        };
        assert!(
            run_update_prompt_if_needed_with_io_and_source(
                &config,
                false,
                true,
                InstallSource::Npm,
                &mut input,
                &mut output,
            )
            .is_err()
        );

        for needle in [b"https://github.com".as_slice(), b"runs `npm".as_slice()] {
            let dir = tempfile::tempdir().expect("tempdir");
            let config = UpdateConfig {
                codex_home: dir.path().to_path_buf(),
                check_for_update_on_startup: true,
            };
            seed_version_info(&config, "99.0.0");
            let mut input = std::io::Cursor::new("2\n");
            let mut output = FailingWriter {
                writes: 0,
                fail_write: None,
                fail_contains: Some(needle),
                fail_flush: false,
            };
            let result = run_update_prompt_if_needed_with_io_and_source(
                &config,
                false,
                true,
                InstallSource::Npm,
                &mut input,
                &mut output,
            );
            assert!(
                result.is_err(),
                "needle: {:?}, writes: {}",
                needle,
                output.writes
            );
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::Cursor::new("2\n");
        let mut output = FailingWriter {
            writes: 0,
            fail_write: None,
            fail_contains: None,
            fail_flush: false,
        };
        assert!(
            run_update_prompt_if_needed_with_io_and_source(
                &config,
                false,
                true,
                InstallSource::Npm,
                &mut input,
                &mut output,
            )
            .is_ok()
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        seed_version_info(&config, "99.0.0");
        let mut input = std::io::BufReader::new(FailingReader);
        let mut output = Vec::new();
        let err = run_update_prompt_if_needed_with_io_and_source(
            &config,
            false,
            true,
            InstallSource::Npm,
            &mut input,
            &mut output,
        )
        .unwrap_err();
        assert!(err.contains("Could not read update choice"));
    }

    #[test]
    fn update_cache_defaults_and_empty_or_legacy_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for_update(dir.path().to_path_buf());
        fs::create_dir_all(&paths.profiles).unwrap();
        fs::write(&paths.profiles_lock, "").unwrap();
        fs::write(&paths.update_cache, "{\"latest_version\":\"1.2.3\"}").unwrap();
        let cache = read_update_cache(&paths).unwrap().unwrap();
        assert_eq!(cache.last_checked_at, update_cache_checked_default());

        fs::write(&paths.update_cache, "\n").unwrap();
        assert!(read_update_cache(&paths).unwrap().is_none());

        fs::remove_file(&paths.update_cache).unwrap();
        fs::write(&paths.profiles_index, "{\"version\":2,\"profiles\":{}}").unwrap();
        assert!(read_update_cache(&paths).unwrap().is_none());
    }

    #[test]
    fn update_cache_read_error_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for_update(dir.path().to_path_buf());
        fs::create_dir_all(&paths.profiles).unwrap();
        fs::write(&paths.profiles_lock, "").unwrap();

        fs::create_dir(&paths.update_cache).unwrap();
        assert!(read_update_cache(&paths).is_err());
        fs::remove_dir(&paths.update_cache).unwrap();
        fs::write(&paths.update_cache, "not json").unwrap();
        assert!(read_update_cache(&paths).is_err());
        fs::remove_file(&paths.update_cache).unwrap();

        fs::create_dir(&paths.profiles_index).unwrap();
        assert!(read_update_cache(&paths).is_err());
        fs::remove_dir(&paths.profiles_index).unwrap();
        fs::write(&paths.profiles_index, "not json").unwrap();
        assert!(read_update_cache(&paths).is_err());

        let legacy = serde_json::json!({
            "update_cache": {"latest_version": 7}
        });
        fs::write(
            &paths.profiles_index,
            serde_json::to_string(&legacy).unwrap(),
        )
        .unwrap();
        assert!(read_update_cache(&paths).is_err());

        let legacy = serde_json::json!({
            "update_cache": {
                "latest_version": "1.2.3",
                "last_checked_at": "2024-01-01T00:00:00Z"
            },
            "profiles": "invalid"
        });
        fs::write(
            &paths.profiles_index,
            serde_json::to_string(&legacy).unwrap(),
        )
        .unwrap();
        let migrated = read_update_cache(&paths).unwrap().unwrap();
        assert_eq!(migrated.latest_version, "1.2.3");
    }

    #[test]
    fn update_version_fetch_and_refresh_paths() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let release_body = "{\"tag_name\":\"v99.0.0\"}";
        let release_url = spawn_server(http_ok_response(release_body, "application/json"));
        let _env = set_env_guard(LATEST_RELEASE_URL_OVERRIDE_ENV_VAR, Some(&release_url));
        let dir = tempfile::tempdir().expect("tempdir");
        let config = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        let initial_paths = paths_for_update(config.codex_home.clone());
        fs::create_dir_all(&initial_paths.profiles).unwrap();
        fs::write(&initial_paths.profiles_lock, "").unwrap();
        assert_eq!(
            get_upgrade_version_with_debug(&config, false).as_deref(),
            Some("99.0.0")
        );

        let paths = paths_for_update(config.codex_home.clone());
        let mut stale = read_update_cache(&paths).unwrap().unwrap();
        stale.last_checked_at = Utc::now() - Duration::hours(21);
        write_update_cache(&paths, &stale).unwrap();
        let refresh_url = spawn_server(http_ok_response(release_body, "application/json"));
        let _refresh_env = set_env_guard(LATEST_RELEASE_URL_OVERRIDE_ENV_VAR, Some(&refresh_url));
        assert_eq!(
            get_upgrade_version_with_debug(&config, false).as_deref(),
            Some("99.0.0")
        );
        let deadline = std::time::Instant::now() + StdDuration::from_secs(5);
        let refreshed = loop {
            let value = read_update_cache(&paths).unwrap().unwrap();
            if value.last_checked_at > stale.last_checked_at {
                break value;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "background update refresh did not finish"
            );
            std::thread::sleep(StdDuration::from_millis(5));
        };
        assert_eq!(refreshed.latest_version, "99.0.0");

        let mut unknown_input = std::io::Cursor::new("");
        let mut unknown_output = Vec::new();
        let result = run_update_prompt_if_needed_with_io_and_source(
            &config,
            false,
            false,
            InstallSource::Unknown,
            &mut unknown_input,
            &mut unknown_output,
        )
        .unwrap();
        assert_eq!(result, UpdatePromptOutcome::Continue);

        let current = current_version().to_string();
        let mut current_info = read_update_cache(&paths).unwrap().unwrap();
        current_info.latest_version = current;
        current_info.dismissed_version = Some(current_info.latest_version.clone());
        current_info.last_checked_at = Utc::now();
        write_update_cache(&paths, &current_info).unwrap();
        assert!(get_upgrade_version_for_popup_with_debug(&config, false).is_none());

        current_info.latest_version = "99.0.0".to_string();
        current_info.dismissed_version = Some("99.0.0".to_string());
        write_update_cache(&paths, &current_info).unwrap();
        assert!(get_upgrade_version_for_popup_with_debug(&config, false).is_none());
    }

    #[test]
    fn update_fetch_error_and_brew_fallback_paths() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let not_found = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        let cask_404 = spawn_server(not_found.to_string());
        let _cask_env = set_env_guard(HOMEBREW_CASK_URL_OVERRIDE_ENV_VAR, Some(&cask_404));
        assert!(fetch_version_from_cask().is_none());
        let cask_error = spawn_server(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".to_string(),
        );
        let _cask_error_env = set_env_guard(HOMEBREW_CASK_URL_OVERRIDE_ENV_VAR, Some(&cask_error));
        assert!(fetch_version_from_cask().is_none());

        let release_error = spawn_server(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".to_string(),
        );
        let _release_env = set_env_guard(LATEST_RELEASE_URL_OVERRIDE_ENV_VAR, Some(&release_error));
        assert!(fetch_version_from_release().is_none());

        let stale_dir = tempfile::tempdir().expect("tempdir");
        let stale_paths = paths_for_update(stale_dir.path().to_path_buf());
        fs::create_dir_all(&stale_paths.profiles).unwrap();
        fs::write(&stale_paths.profiles_lock, "").unwrap();
        let mut stale = build_update_cache(Some("99.0.0".to_string()), None, None);
        stale.last_checked_at = Utc::now() - Duration::hours(21);
        write_update_cache(&stale_paths, &stale).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&stale_paths.profiles, fs::Permissions::from_mode(0o500)).unwrap();
        }
        let error_refresh = spawn_server(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".to_string(),
        );
        let _error_refresh_env =
            set_env_guard(LATEST_RELEASE_URL_OVERRIDE_ENV_VAR, Some(&error_refresh));
        assert_eq!(
            get_upgrade_version_with_debug(
                &UpdateConfig {
                    codex_home: stale_dir.path().to_path_buf(),
                    check_for_update_on_startup: true,
                },
                false,
            )
            .as_deref(),
            Some("99.0.0")
        );
        std::thread::sleep(std::time::Duration::from_millis(500));

        let cask_ok = spawn_server(http_ok_response("version \"99.0.0\"", "text/plain"));
        let _cask_ok_env = set_env_guard(HOMEBREW_CASK_URL_OVERRIDE_ENV_VAR, Some(&cask_ok));
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_for_update(dir.path().to_path_buf());
        fs::create_dir_all(&paths.profiles).unwrap();
        fs::write(&paths.profiles_lock, "").unwrap();
        check_for_update_with_action(&paths, Some(UpdateAction::BrewUpgrade)).unwrap();
        assert!(
            fs::read_to_string(&paths.update_cache)
                .unwrap()
                .contains("99.0.0")
        );
    }

    #[test]
    fn dismiss_and_prompt_persistence_error_paths() {
        let disabled = UpdateConfig {
            codex_home: PathBuf::new(),
            check_for_update_on_startup: false,
        };
        assert!(dismiss_version(&disabled, "1.2.3").is_ok());
        assert!(mark_prompted_now(&disabled).is_ok());

        let dir = tempfile::tempdir().expect("tempdir");
        let enabled = UpdateConfig {
            codex_home: dir.path().to_path_buf(),
            check_for_update_on_startup: true,
        };
        assert!(dismiss_version(&enabled, "1.2.3").is_ok());
        assert!(mark_prompted_now(&enabled).is_ok());

        seed_version_info(&enabled, "99.0.0");
        let paths = paths_for_update(enabled.codex_home.clone());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&paths.profiles, fs::Permissions::from_mode(0o500)).unwrap();
        }
        #[cfg(not(unix))]
        {
            fs::remove_file(&paths.profiles_lock).unwrap();
            fs::create_dir(&paths.profiles_lock).unwrap();
        }
        assert!(dismiss_version(&enabled, "99.0.0").is_err());
        let mut input = std::io::Cursor::new("3\n");
        let mut output = Vec::new();
        let result = run_update_prompt_if_needed_with_io_and_source(
            &enabled,
            false,
            true,
            InstallSource::Npm,
            &mut input,
            &mut output,
        )
        .unwrap();
        assert_eq!(result, UpdatePromptOutcome::Continue);
        assert!(!output.is_empty());

        let mut input = std::io::Cursor::new("3\n");
        let mut output = FailingWriter {
            writes: 0,
            fail_write: None,
            fail_contains: Some(b"Failed to persist"),
            fail_flush: false,
        };
        let result = run_update_prompt_if_needed_with_io_and_source(
            &enabled,
            false,
            true,
            InstallSource::Npm,
            &mut input,
            &mut output,
        );
        assert!(result.is_err());
    }
}
