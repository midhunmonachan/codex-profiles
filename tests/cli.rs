mod common;

use common::build_id_token;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

const ALPHA_ACCOUNT: &str = "acct-alpha";
const ALPHA_EMAIL: &str = "alpha@example.com";
const ALPHA_PLAN: &str = "team";
const ALPHA_TOKEN: &str = "token-alpha";
const ALPHA_ID: &str = "alpha@example.com-team";
const BETA_ACCOUNT: &str = "acct-beta";
const BETA_EMAIL: &str = "beta@example.com";
const BETA_PLAN: &str = "team";
const BETA_TOKEN: &str = "token-beta";
const BETA_ID: &str = "beta@example.com-team";
const FREE_ACCOUNT: &str = "acct-free";
const FREE_EMAIL: &str = "free@example.com";
const FREE_PLAN: &str = "free";
const FREE_TOKEN: &str = "token-free";

static COMMAND_MUTEX: Mutex<()> = Mutex::new(());

struct TestEnv {
    home: tempfile::TempDir,
    bin_path: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::Builder::new()
            .prefix("codex-profiles-test-")
            .tempdir()
            .expect("create temp home");
        fs::create_dir_all(home.path().join(".codex")).expect("create codex dir");

        let source_bin = resolve_bin_path();
        let bin_dir = home.path().join(".test-bin");
        fs::create_dir_all(&bin_dir).expect("create test bin dir");
        let bin_name = source_bin.file_name().expect("binary file name");
        let bin_path = bin_dir.join(bin_name);
        fs::copy(&source_bin, &bin_path).expect("copy test binary");
        let permissions = fs::metadata(&source_bin)
            .expect("source binary metadata")
            .permissions();
        fs::set_permissions(&bin_path, permissions).expect("set test binary permissions");

        Self { home, bin_path }
    }

    fn home_path(&self) -> &Path {
        self.home.path()
    }

    fn codex_dir(&self) -> PathBuf {
        self.home_path().join(".codex")
    }

    fn profiles_dir(&self) -> PathBuf {
        self.codex_dir().join("profiles")
    }

    fn write_config(&self, base_url: &str) {
        let path = self.codex_dir().join("config.toml");
        let contents = format!("chatgpt_base_url = \"{}\"\n", base_url);
        fs::write(path, contents).expect("write config");
    }

    fn write_auth_base(
        &self,
        account_id: &str,
        email: &str,
        plan: &str,
        access_token: &str,
        refresh_token: Option<&str>,
    ) {
        let id_token = build_id_token(email, plan);
        let mut tokens = serde_json::Map::new();
        tokens.insert(
            "account_id".to_string(),
            serde_json::Value::String(account_id.to_string()),
        );
        tokens.insert("id_token".to_string(), serde_json::Value::String(id_token));
        tokens.insert(
            "access_token".to_string(),
            serde_json::Value::String(access_token.to_string()),
        );
        if let Some(refresh_token) = refresh_token {
            tokens.insert(
                "refresh_token".to_string(),
                serde_json::Value::String(refresh_token.to_string()),
            );
        }
        let value = serde_json::Value::Object({
            let mut root = serde_json::Map::new();
            root.insert("tokens".to_string(), serde_json::Value::Object(tokens));
            root
        });
        let path = self.codex_dir().join("auth.json");
        fs::write(path, serde_json::to_string(&value).expect("serialize auth"))
            .expect("write auth.json");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                self.codex_dir().join("auth.json"),
                fs::Permissions::from_mode(0o600),
            )
            .expect("chmod auth.json");
        }
    }

    fn write_auth(&self, account_id: &str, email: &str, plan: &str, access_token: &str) {
        self.write_auth_base(account_id, email, plan, access_token, None);
    }

    fn write_auth_with_refresh(
        &self,
        account_id: &str,
        email: &str,
        plan: &str,
        access_token: &str,
        refresh_token: &str,
    ) {
        self.write_auth_base(account_id, email, plan, access_token, Some(refresh_token));
    }

    fn write_profiles_index(
        &self,
        entries: &[(&str, u64)],
        labels: &[(&str, &str)],
        active_id: Option<&str>,
    ) {
        fs::create_dir_all(self.profiles_dir()).expect("create profiles dir");
        let mut profiles = serde_json::Map::new();
        let label_map: std::collections::HashMap<_, _> = labels.iter().copied().collect();
        for (id, _last_used) in entries {
            let mut entry = serde_json::Map::new();
            entry.insert("added_at".to_string(), serde_json::json!(1));
            if let Some(label) = label_map.get(id) {
                entry.insert("label".to_string(), serde_json::json!(label));
            }
            profiles.insert(id.to_string(), serde_json::Value::Object(entry));
        }
        let index = serde_json::json!({
            "version": 1,
            "active_profile_id": active_id,
            "profiles": serde_json::Value::Object(profiles)
        });
        let path = self.profiles_dir().join("profiles.json");
        fs::write(
            path,
            serde_json::to_string(&index).expect("serialize profiles.json"),
        )
        .expect("write profiles.json");
    }

    fn read_auth(&self) -> String {
        let path = self.codex_dir().join("auth.json");
        fs::read_to_string(path).expect("read auth.json")
    }

    fn run(&self, args: &[&str]) -> String {
        let output = self.run_output(args);
        self.assert_success(args, output)
    }

    fn run_expect_error(&self, args: &[&str]) -> String {
        let output = self.run_output(args);
        if output.status.success() {
            panic!(
                "command unexpectedly succeeded: {:?}\nstdout:\n{}",
                args,
                String::from_utf8_lossy(&output.stdout)
            );
        }
        ascii_only(String::from_utf8_lossy(&output.stderr).as_ref())
    }

    fn run_output(&self, args: &[&str]) -> Output {
        self.run_output_with_env(args, &[])
    }

    fn run_output_with_env(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let _guard = COMMAND_MUTEX.lock().expect("command mutex");
        let mut cmd = Command::new(&self.bin_path);
        cmd.args(args)
            .env("HOME", self.home_path())
            .env("CODEX_PROFILES_HOME", self.home_path())
            .env("CODEX_PROFILES_COMMAND", "codex-profiles")
            .env("CODEX_PROFILES_SKIP_UPDATE", "1")
            .env("NO_COLOR", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::null());
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        if cfg!(windows) {
            cmd.env("USERPROFILE", self.home_path());
            if let Some(home_str) = self.home_path().to_str()
                && let Some(idx) = home_str.find(':')
            {
                let (drive, rest) = home_str.split_at(idx + 1);
                cmd.env("HOMEDRIVE", drive);
                cmd.env("HOMEPATH", rest);
            }
        }
        cmd.output().expect("run command")
    }

    fn run_with_env(&self, args: &[&str], extra_env: &[(&str, &str)]) -> String {
        let output = self.run_output_with_env(args, extra_env);
        self.assert_success(args, output)
    }

    fn assert_success(&self, args: &[&str], output: Output) -> String {
        if !output.status.success() {
            panic!(
                "command failed: {:?}\nstdout:\n{}\nstderr:\n{}",
                args,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        ascii_only(String::from_utf8_lossy(&output.stdout).as_ref())
    }
}

fn seed_alpha(env: &TestEnv) {
    env.write_auth(ALPHA_ACCOUNT, ALPHA_EMAIL, ALPHA_PLAN, ALPHA_TOKEN);
}

fn seed_alpha_with_token(env: &TestEnv, token: &str) {
    env.write_auth(ALPHA_ACCOUNT, ALPHA_EMAIL, ALPHA_PLAN, token);
}

fn seed_beta(env: &TestEnv) {
    env.write_auth(BETA_ACCOUNT, BETA_EMAIL, BETA_PLAN, BETA_TOKEN);
}

fn seed_free(env: &TestEnv) {
    env.write_auth(FREE_ACCOUNT, FREE_EMAIL, FREE_PLAN, FREE_TOKEN);
}

fn seed_current(env: &TestEnv) {
    env.write_auth(
        "acct-current",
        "current@example.com",
        "team",
        "token-current",
    );
}

fn ascii_only(raw: &str) -> String {
    let output = raw.replace('\r', "");
    let filtered: String = output.chars().filter(|ch| ch.is_ascii()).collect();
    filtered
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn profile_label(env: &TestEnv, id: &str) -> Option<String> {
    let index_path = env.profiles_dir().join("profiles.json");
    let index = fs::read_to_string(index_path).expect("read profiles.json");
    let json: serde_json::Value = serde_json::from_str(&index).expect("parse profiles.json");
    json.get("profiles")
        .and_then(|profiles| profiles.get(id))
        .and_then(|entry| entry.get("label"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn read_json_file(path: &PathBuf) -> serde_json::Value {
    let raw = fs::read_to_string(path).expect("read json file");
    serde_json::from_str(&raw).expect("parse json file")
}

fn resolve_bin_path() -> PathBuf {
    if let Ok(path) = env::var("CARGO_BIN_EXE_codex-profiles") {
        let path = PathBuf::from(path);
        if path.exists() {
            return path;
        }
    }
    let exe = env::current_exe().expect("current exe");
    let target_dir = exe
        .parent()
        .and_then(|path| path.parent())
        .expect("target dir");
    let bin_name = if cfg!(windows) {
        "codex-profiles.exe"
    } else {
        "codex-profiles"
    };
    target_dir.join(bin_name)
}

#[test]
fn test_env_uses_unique_temp_dirs_and_binary_copies() {
    let first = TestEnv::new();
    let second = TestEnv::new();

    assert_ne!(first.home_path(), second.home_path());
    assert_ne!(first.bin_path, second.bin_path);
    assert!(first.bin_path.is_file());
    assert!(second.bin_path.is_file());
}

fn seed_profiles(env: &TestEnv) {
    seed_alpha(env);
    env.run(&["save", "--label", "alpha"]);
    seed_beta(env);
    env.run(&["save", "--label", "beta"]);
}

fn start_usage_server(
    body: &'static str,
    max_requests: usize,
) -> std::io::Result<(SocketAddr, thread::JoinHandle<()>)> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    start_response_server(vec![response], max_requests)
}

fn start_response_server(
    responses: Vec<String>,
    max_requests: usize,
) -> std::io::Result<(SocketAddr, thread::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let responses: Vec<Vec<u8>> = responses.into_iter().map(String::into_bytes).collect();
    let handle = thread::spawn(move || {
        let mut handled = 0usize;
        let mut last_activity = Instant::now();
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf);
                    if responses.is_empty() {
                        break;
                    }
                    let idx = handled.min(responses.len() - 1);
                    let _ = stream.write_all(&responses[idx]);
                    handled += 1;
                    last_activity = Instant::now();
                    if handled >= max_requests {
                        break;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    // Give parallel tests enough time to acquire the command
                    // mutex and issue their first request on slower CI hosts.
                    // Keep post-request timeout shorter to avoid hangs.
                    let idle_timeout = if handled == 0 {
                        Duration::from_secs(60)
                    } else {
                        Duration::from_secs(10)
                    };
                    if last_activity.elapsed() > idle_timeout {
                        break;
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });
    Ok((addr, handle))
}

fn assert_status_output(env: &TestEnv, args: &[&str], expected_profiles: &[&str]) {
    let body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000},"secondary_window":{"used_percent":50,"limit_window_seconds":604800,"reset_at":2000600000}}}"#;
    let server = start_usage_server(body, 6);
    if let Ok((addr, handle)) = server {
        env.write_config(&format!("http://{addr}/backend-api"));
        let output = env.run(args);
        assert_contains_all(&output, expected_profiles);
        if !output.contains("resets ") {
            assert!(output.contains("Error:"));
        }
        let _ = handle.join();
    } else {
        env.write_config("http://127.0.0.1:1/backend-api");
        let output = env.run(args);
        assert_contains_all(&output, expected_profiles);
        assert!(output.contains("Error:"));
    }
}

fn assert_contains_all(output: &str, expected: &[&str]) {
    for name in expected {
        assert!(output.contains(name));
    }
}

fn assert_order(output: &str, first: &str, second: &str) {
    let first_idx = output
        .find(first)
        .unwrap_or_else(|| panic!("missing expected text: {first}"));
    let second_idx = output
        .find(second)
        .unwrap_or_else(|| panic!("missing expected text: {second}"));
    assert!(
        first_idx < second_idx,
        "expected '{first}' before '{second}' in output"
    );
}

fn assert_profile_block_layout(output: &str, email: &str, next_email: Option<&str>) {
    let lines: Vec<&str> = output.lines().collect();
    let header_idx = lines
        .iter()
        .position(|line| line.contains(email))
        .unwrap_or_else(|| panic!("missing profile header for {email}"));
    assert_eq!(lines.get(header_idx + 1), Some(&""));
    assert!(
        lines
            .get(header_idx + 2)
            .is_some_and(|line| line.contains("5 hour:"))
    );
    assert!(
        lines
            .get(header_idx + 3)
            .is_some_and(|line| line.contains("Weekly:"))
    );
    match next_email {
        Some(next) => {
            assert_eq!(lines.get(header_idx + 4), Some(&""));
            assert_eq!(lines.get(header_idx + 5), Some(&""));
            assert!(
                lines
                    .get(header_idx + 6)
                    .is_some_and(|line| line.contains(next))
            );
        }
        None => {
            assert!(lines.get(header_idx + 4).is_none());
        }
    }
}

fn write_profile_tokens(env: &TestEnv, id: &str, tokens: serde_json::Value) {
    fs::create_dir_all(env.profiles_dir()).expect("create profiles dir");
    let value = serde_json::json!({ "tokens": tokens });
    let path = env.profiles_dir().join(format!("{id}.json"));
    fs::write(
        path,
        serde_json::to_string(&value).expect("serialize profile"),
    )
    .expect("write profile");
}

fn write_auth_tokens(env: &TestEnv, tokens: serde_json::Value) {
    let value = serde_json::json!({ "tokens": tokens });
    let path = env.codex_dir().join("auth.json");
    fs::write(path, serde_json::to_string(&value).expect("serialize auth")).expect("write auth");
}

fn seed_api_profile(env: &TestEnv, id: &str, account_id: &str) {
    write_profile_tokens(
        env,
        id,
        serde_json::json!({
            "account_id": account_id,
        }),
    );
}

fn seed_errored_profile(env: &TestEnv, id: &str) {
    write_profile_tokens(
        env,
        id,
        serde_json::json!({
            "account_id": "acct-errored",
            "refresh_token": "refresh-only"
        }),
    );
}

#[test]
fn ui_save_command() {
    let env = TestEnv::new();
    seed_alpha(&env);
    let output = env.run(&["save", "--label", "alpha"]);
    assert!(output.contains("Saved profile"));
    assert!(output.contains("alpha@example.com"));
    let profile_path = env.profiles_dir().join(format!("{ALPHA_ID}.json"));
    assert!(profile_path.is_file());
}

#[cfg(unix)]
#[test]
fn ui_save_command_writes_private_files() {
    use std::os::unix::fs::PermissionsExt;

    let env = TestEnv::new();
    seed_alpha(&env);
    fs::set_permissions(
        env.codex_dir().join("auth.json"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("set permissive auth mode");

    env.run(&["save", "--label", "alpha"]);

    let profile_mode = fs::metadata(env.profiles_dir().join(format!("{ALPHA_ID}.json")))
        .expect("profile metadata")
        .permissions()
        .mode()
        & 0o777;
    let index_mode = fs::metadata(env.profiles_dir().join("profiles.json"))
        .expect("index metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(profile_mode, 0o600);
    assert_eq!(index_mode, 0o600);
}

#[test]
fn ui_save_missing_auth() {
    let env = TestEnv::new();
    let err = env.run_expect_error(&["save"]);
    assert!(err.contains("Auth file not found"));
}

#[test]
fn ui_save_empty_label() {
    let env = TestEnv::new();
    seed_alpha(&env);
    let err = env.run_expect_error(&["save", "--label", "   "]);
    assert!(err.contains("Label cannot be empty"));
}

#[test]
fn ui_save_trims_label() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "  work  "]);
    let index_path = env.profiles_dir().join("profiles.json");
    let index = fs::read_to_string(index_path).expect("read profiles.json");
    let json: serde_json::Value = serde_json::from_str(&index).expect("parse profiles.json");
    let label = json
        .get("profiles")
        .and_then(|profiles| profiles.get(ALPHA_ID))
        .and_then(|entry| entry.get("label"))
        .and_then(|value| value.as_str());
    assert_eq!(label, Some("work"));
}

#[test]
fn ui_save_renames_primary_candidate_to_canonical_id() {
    let env = TestEnv::new();
    seed_alpha(&env);
    fs::create_dir_all(env.profiles_dir()).expect("create profiles dir");
    let profile_one = env.profiles_dir().join(format!("{ALPHA_ID}-old.json"));
    fs::copy(env.codex_dir().join("auth.json"), &profile_one).expect("seed profile one");
    seed_alpha_with_token(&env, "token-alpha-rotated");
    let profile_two = env.profiles_dir().join(format!("{ALPHA_ID}-alt.json"));
    fs::copy(env.codex_dir().join("auth.json"), &profile_two).expect("seed profile two");
    env.write_profiles_index(&[], &[], None);
    seed_alpha_with_token(&env, "token-alpha-new");
    env.run(&["save"]);
    assert!(profile_one.is_file());
    assert!(!profile_two.is_file());
    let canonical = env.profiles_dir().join(format!("{ALPHA_ID}.json"));
    assert!(canonical.is_file());
}

#[test]
fn ui_save_duplicate_label() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);
    seed_beta(&env);
    let err = env.run_expect_error(&["save", "--label", "alpha"]);
    assert!(err.contains("Label 'alpha' already exists"));
}

#[test]
fn ui_save_without_label_shows_label_hint() {
    let env = TestEnv::new();
    seed_alpha(&env);
    let output = env.run(&["save"]);
    assert!(output.contains("Saved profile"));
    assert!(output.contains("label set --id"));
    assert!(output.contains(ALPHA_ID));
}

#[test]
fn ui_label_set_by_id_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["label", "set", "--id", BETA_ID, "--to", "work"]);
    assert!(output.contains("Set label 'work'"));
    assert!(output.contains(BETA_ID));
    assert_eq!(profile_label(&env, BETA_ID), Some("work".to_string()));
}

#[test]
fn ui_label_set_by_label_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["label", "set", "--label", "alpha", "--to", "personal"]);
    assert!(output.contains("Set label 'personal'"));
    assert!(output.contains(ALPHA_ID));
    assert_eq!(profile_label(&env, ALPHA_ID), Some("personal".to_string()));
}

#[test]
fn ui_label_clear_by_label_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["label", "clear", "--label", "beta"]);
    assert!(output.contains("Cleared label"));
    assert!(output.contains(BETA_ID));
    assert_eq!(profile_label(&env, BETA_ID), None);
}

#[test]
fn ui_label_set_conflict() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let err = env.run_expect_error(&["label", "set", "--id", BETA_ID, "--to", "alpha"]);
    assert!(err.contains("Label 'alpha' already exists"));
}

#[test]
fn ui_label_set_empty_label() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let err = env.run_expect_error(&["label", "set", "--id", BETA_ID, "--to", "   "]);
    assert!(err.contains("Label cannot be empty"));
}

#[test]
fn ui_label_set_id_not_found() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let err = env.run_expect_error(&["label", "set", "--id", "missing-id", "--to", "work"]);
    assert!(err.contains("Profile id 'missing-id'"));
}

#[test]
fn ui_label_clear_label_not_found() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let err = env.run_expect_error(&["label", "clear", "--label", "missing"]);
    assert!(err.contains("Label 'missing' was not found"));
}

#[test]
fn ui_label_rename_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["label", "rename", "--label", "alpha", "--to", "work"]);
    assert!(output.contains("Renamed label 'alpha' to 'work'"));
    assert_eq!(profile_label(&env, ALPHA_ID), Some("work".to_string()));
}

#[test]
fn ui_default_command_is_unrecognized() {
    let env = TestEnv::new();
    let err = env.run_expect_error(&["default", "show"]);
    assert!(err.contains("unrecognized subcommand 'default'"));
}

#[test]
fn ui_load_ignores_legacy_default_when_notty() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let index_path = env.profiles_dir().join("profiles.json");
    let mut index = read_json_file(&index_path);
    index["default_profile_id"] = serde_json::json!(BETA_ID);
    fs::write(
        &index_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&index).expect("serialize legacy index")
        ),
    )
    .expect("write legacy index");

    let err = env.run_expect_error(&["load"]);
    assert!(err.contains("load selection requires a TTY"));
}

#[test]
fn ui_export_all_profiles_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let export_path = env.home_path().join("profiles-export.json");
    let output = env.run(&["export", "--output", export_path.to_str().unwrap()]);
    assert!(output.contains("Exported 2 profiles"));

    let json = read_json_file(&export_path);
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert_eq!(json.get("version").unwrap(), &serde_json::json!(1));
    assert!(json.get("default_profile_id").is_none());
    assert_eq!(profiles.len(), 2);
    assert!(profiles.iter().any(|profile| {
        profile.get("id") == Some(&serde_json::json!(ALPHA_ID))
            && profile.get("label") == Some(&serde_json::json!("alpha"))
    }));
    assert!(profiles.iter().any(|profile| {
        profile.get("id") == Some(&serde_json::json!(BETA_ID))
            && profile.get("label") == Some(&serde_json::json!("beta"))
    }));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&export_path)
            .expect("export metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn ui_export_selected_id_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let export_path = env.home_path().join("profiles-export-alpha.json");
    let output = env.run(&[
        "export",
        "--id",
        ALPHA_ID,
        "--output",
        export_path.to_str().unwrap(),
    ]);
    assert!(output.contains("Exported 1 profile"));

    let json = read_json_file(&export_path);
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].get("id").unwrap(), &serde_json::json!(ALPHA_ID));
}

#[test]
fn ui_export_selected_label_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let export_path = env.home_path().join("profiles-export-beta.json");
    let output = env.run(&[
        "export",
        "--label",
        "beta",
        "--output",
        export_path.to_str().unwrap(),
    ]);
    assert!(output.contains("Exported 1 profile"));

    let json = read_json_file(&export_path);
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert_eq!(profiles.len(), 1);
    assert!(json.get("default_profile_id").is_none());
    assert_eq!(profiles[0].get("id").unwrap(), &serde_json::json!(BETA_ID));
    assert_eq!(
        profiles[0].get("label").unwrap(),
        &serde_json::json!("beta")
    );
}

#[test]
fn ui_import_profiles_command() {
    let src = TestEnv::new();
    seed_profiles(&src);
    let export_path = src.home_path().join("profiles-export.json");
    src.run(&["export", "--output", export_path.to_str().unwrap()]);

    let dest = TestEnv::new();
    let output = dest.run(&["import", "--input", export_path.to_str().unwrap()]);
    assert!(output.contains("Imported 2 profiles"));
    let index = read_json_file(&dest.profiles_dir().join("profiles.json"));
    assert!(index.get("default_profile_id").is_none());

    let json: serde_json::Value =
        serde_json::from_str(&dest.run(&["list", "--json"])).expect("parse list json");
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert_eq!(profiles.len(), 2);
    assert!(
        profiles
            .iter()
            .any(|profile| profile.get("id") == Some(&serde_json::json!(ALPHA_ID)))
    );
    assert!(profiles.iter().any(|profile| {
        profile.get("id") == Some(&serde_json::json!(ALPHA_ID))
            && profile.get("label") == Some(&serde_json::json!("alpha"))
    }));
    assert!(
        profiles
            .iter()
            .any(|profile| profile.get("id") == Some(&serde_json::json!(BETA_ID)))
    );
    assert!(profiles.iter().any(|profile| {
        profile.get("id") == Some(&serde_json::json!(BETA_ID))
            && profile.get("label") == Some(&serde_json::json!("beta"))
    }));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for id in [ALPHA_ID, BETA_ID] {
            let path = dest.profiles_dir().join(format!("{id}.json"));
            let mode = fs::metadata(path)
                .expect("profile metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}

#[test]
fn ui_import_rejects_existing_id_conflict() {
    let src = TestEnv::new();
    seed_profiles(&src);
    let export_path = src.home_path().join("profiles-export.json");
    src.run(&["export", "--output", export_path.to_str().unwrap()]);

    let dest = TestEnv::new();
    seed_beta(&dest);
    dest.run(&["save", "--label", "beta"]);
    let err = dest.run_expect_error(&["import", "--input", export_path.to_str().unwrap()]);
    assert!(err.contains(BETA_ID));
    assert!(err.contains("already exists"));

    let json: serde_json::Value =
        serde_json::from_str(&dest.run(&["list", "--json"])).expect("parse list json");
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].get("id").unwrap(), &serde_json::json!(BETA_ID));
}

#[test]
fn ui_import_rejects_existing_label_conflict() {
    let src = TestEnv::new();
    seed_alpha(&src);
    src.run(&["save", "--label", "alpha"]);
    let export_path = src.home_path().join("profiles-export-alpha.json");
    src.run(&[
        "export",
        "--id",
        ALPHA_ID,
        "--output",
        export_path.to_str().unwrap(),
    ]);

    let dest = TestEnv::new();
    seed_beta(&dest);
    dest.run(&["save", "--label", "beta"]);
    dest.run(&["label", "set", "--id", BETA_ID, "--to", "alpha"]);
    let err = dest.run_expect_error(&["import", "--input", export_path.to_str().unwrap()]);
    assert!(err.contains("Label 'alpha' already exists"));

    let json: serde_json::Value =
        serde_json::from_str(&dest.run(&["list", "--json"])).expect("parse list json");
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].get("id").unwrap(), &serde_json::json!(BETA_ID));
    assert_eq!(
        profiles[0].get("label").unwrap(),
        &serde_json::json!("alpha")
    );
}

#[test]
fn ui_import_legacy_bundle_without_default_field() {
    let src = TestEnv::new();
    seed_profiles(&src);
    let export_path = src.home_path().join("profiles-export.json");
    src.run(&["export", "--output", export_path.to_str().unwrap()]);

    let mut json = read_json_file(&export_path);
    json.as_object_mut()
        .expect("bundle object")
        .remove("default_profile_id");
    fs::write(
        &export_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json).expect("serialize legacy bundle")
        ),
    )
    .expect("write legacy bundle");

    let dest = TestEnv::new();
    let output = dest.run(&["import", "--input", export_path.to_str().unwrap()]);
    assert!(output.contains("Imported 2 profiles"));
    let index = read_json_file(&dest.profiles_dir().join("profiles.json"));
    assert!(index.get("default_profile_id").is_none());
}

#[test]
fn ui_import_rejects_unsafe_profile_id() {
    let src = TestEnv::new();
    seed_alpha(&src);
    src.run(&["save", "--label", "alpha"]);
    let export_path = src.home_path().join("profiles-export-alpha.json");
    src.run(&[
        "export",
        "--id",
        ALPHA_ID,
        "--output",
        export_path.to_str().unwrap(),
    ]);

    let mut json = read_json_file(&export_path);
    json["profiles"][0]["id"] = serde_json::json!("../auth");
    fs::write(
        &export_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json).expect("serialize tampered export")
        ),
    )
    .expect("write tampered export");

    let dest = TestEnv::new();
    let err = dest.run_expect_error(&["import", "--input", export_path.to_str().unwrap()]);
    assert!(err.contains("Imported profile id '../auth' is not safe"));

    let json: serde_json::Value =
        serde_json::from_str(&dest.run(&["list", "--json"])).expect("parse list json");
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert!(profiles.is_empty());
}

#[test]
fn ui_import_rejects_reserved_profile_id() {
    let src = TestEnv::new();
    seed_alpha(&src);
    src.run(&["save", "--label", "alpha"]);
    let export_path = src.home_path().join("profiles-export-alpha.json");
    src.run(&[
        "export",
        "--id",
        ALPHA_ID,
        "--output",
        export_path.to_str().unwrap(),
    ]);

    for reserved in ["profiles", "update"] {
        let mut json = read_json_file(&export_path);
        json["profiles"][0]["id"] = serde_json::json!(reserved);
        fs::write(
            &export_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&json).expect("serialize tampered export")
            ),
        )
        .expect("write tampered export");

        let dest = TestEnv::new();
        let err = dest.run_expect_error(&["import", "--input", export_path.to_str().unwrap()]);
        assert!(err.contains(&format!("Imported profile id '{reserved}' is reserved")));

        let json: serde_json::Value =
            serde_json::from_str(&dest.run(&["list", "--json"])).expect("parse list json");
        let profiles = json
            .get("profiles")
            .and_then(|value| value.as_array())
            .expect("profiles array");
        assert!(profiles.is_empty());
    }
}

#[test]
fn ui_doctor_reports_missing_state() {
    let env = TestEnv::new();
    let output = env.run(&["doctor"]);
    assert!(output.contains("Doctor"));
    assert!(output.contains("[warn] auth file: missing"));
    assert!(output.contains("[info] profiles directory: missing"));
    assert!(output.contains("[info] profiles index: missing"));
    assert!(output.contains("[info] profiles lock: not created yet"));
    assert!(output.contains("[info] active profile: no auth file"));
}

#[test]
fn ui_doctor_reports_unsaved_current_profile() {
    let env = TestEnv::new();
    seed_alpha(&env);
    let output = env.run(&["doctor"]);
    assert!(output.contains("[ok] auth file: valid"));
    assert!(output.contains("[ok] saved profiles: 0 valid, 0 invalid"));
    assert!(output.contains("[warn] active profile: not saved (run `codex-profiles save`)"));
}

#[test]
fn ui_doctor_reports_saved_current_profile() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["doctor"]);
    assert!(output.contains("[ok] auth file: valid"));
    assert!(output.contains("[ok] saved profiles: 2 valid, 0 invalid"));
    assert!(output.contains("[ok] active profile: saved"));
}

#[test]
fn ui_doctor_reports_incomplete_auth() {
    let env = TestEnv::new();
    fs::write(
        env.codex_dir().join("auth.json"),
        serde_json::json!({
            "tokens": {
                "account_id": "acct-partial",
                "access_token": "token-partial"
            }
        })
        .to_string(),
    )
    .expect("write partial auth");
    let output = env.run(&["doctor"]);
    assert!(output.contains("[warn] auth file: present but incomplete"));
    assert!(output.contains("[warn] active profile: unavailable (present but incomplete"));
}

#[test]
fn ui_doctor_reports_invalid_profile_file_and_index() {
    let env = TestEnv::new();
    fs::create_dir_all(env.profiles_dir()).expect("create profiles dir");
    fs::write(env.profiles_dir().join("broken.json"), "{not-json").expect("write broken profile");
    fs::write(env.profiles_dir().join("profiles.json"), "{not-json").expect("write broken index");
    let output = env.run(&["doctor"]);
    assert!(output.contains("[error] profiles index:"));
    assert!(output.contains(
        "[warn] saved profiles: 0 valid, 1 invalid (remove or re-save invalid profiles)"
    ));
}

#[test]
fn ui_doctor_json_missing_state() {
    let env = TestEnv::new();
    let output = env.run(&["doctor", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse doctor json");
    let checks = json
        .get("checks")
        .and_then(|value| value.as_array())
        .expect("checks array");
    let summary = json.get("summary").expect("summary object");

    assert!(checks.iter().any(|check| {
        check.get("name") == Some(&serde_json::json!("auth file"))
            && check.get("level") == Some(&serde_json::json!("warn"))
    }));
    assert!(checks.iter().any(|check| {
        check.get("name") == Some(&serde_json::json!("active profile"))
            && check.get("level") == Some(&serde_json::json!("info"))
    }));
    assert!(json.get("repairs").is_none());
    assert_eq!(summary.get("error").unwrap(), &serde_json::json!(0));
}

#[test]
fn ui_doctor_json_saved_current_profile() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["doctor", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse doctor json");
    let checks = json
        .get("checks")
        .and_then(|value| value.as_array())
        .expect("checks array");

    assert!(checks.iter().any(|check| {
        check.get("name") == Some(&serde_json::json!("saved profiles"))
            && check.get("detail") == Some(&serde_json::json!("2 valid, 0 invalid"))
    }));
    assert!(checks.iter().any(|check| {
        check.get("name") == Some(&serde_json::json!("active profile"))
            && check.get("detail") == Some(&serde_json::json!("saved"))
    }));
    assert!(json.get("repairs").is_none());
}

#[test]
fn ui_doctor_fix_creates_missing_storage() {
    let env = TestEnv::new();
    let output = env.run(&["doctor", "--fix"]);
    assert!(output.contains("Repairs applied:"));
    assert!(output.contains("Created profiles directory"));
    assert!(output.contains("Created profiles lock file"));
    assert!(output.contains("Initialized profiles index"));
    assert!(env.profiles_dir().is_dir());
    assert!(env.profiles_dir().join("profiles.lock").is_file());
    assert!(env.profiles_dir().join("profiles.json").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(env.profiles_dir())
            .expect("profiles dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }
}

#[test]
fn ui_doctor_fix_rebuilds_invalid_index() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);
    fs::write(env.profiles_dir().join("profiles.json"), "{not-json").expect("write broken index");

    let output = env.run(&["doctor", "--fix"]);
    assert!(output.contains("Backed up invalid profiles index"));
    assert!(output.contains("Rebuilt invalid profiles index"));
    assert!(env.profiles_dir().join("profiles.json.bak").is_file());

    let json: serde_json::Value =
        serde_json::from_str(&env.run(&["list", "--json"])).expect("parse list json");
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert!(
        profiles
            .iter()
            .any(|profile| profile.get("id") == Some(&serde_json::json!(ALPHA_ID)))
    );
}

#[test]
fn ui_doctor_fix_rebuilds_invalid_index_without_clobbering_backup() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);
    fs::write(env.profiles_dir().join("profiles.json"), "{not-json").expect("write broken index");
    fs::write(env.profiles_dir().join("profiles.json.bak"), "old-backup")
        .expect("write existing backup");

    let output = env.run(&["doctor", "--fix"]);
    assert!(output.contains("profiles.json.bak.1"));
    assert!(env.profiles_dir().join("profiles.json.bak").is_file());
    assert!(env.profiles_dir().join("profiles.json.bak.1").is_file());
}

#[test]
fn ui_doctor_fix_json_reports_repairs() {
    let env = TestEnv::new();
    let output = env.run(&["doctor", "--fix", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse doctor json");
    let repairs = json
        .get("repairs")
        .and_then(|value| value.as_array())
        .expect("repairs array");
    assert!(!repairs.is_empty());
}

#[test]
fn ui_doctor_fix_noop_reports_no_repairs() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["doctor", "--fix"]);
    assert!(output.contains("No repairs needed."));
}

#[test]
fn ui_doctor_warns_on_permissive_permissions() {
    let env = TestEnv::new();
    seed_profiles(&env);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(env.profiles_dir(), fs::Permissions::from_mode(0o755))
            .expect("set permissive test mode");
        fs::set_permissions(
            env.profiles_dir().join("profiles.json"),
            fs::Permissions::from_mode(0o644),
        )
        .expect("set permissive index mode");
        fs::set_permissions(
            env.profiles_dir().join("profiles.lock"),
            fs::Permissions::from_mode(0o644),
        )
        .expect("set permissive lock mode");
        fs::set_permissions(
            env.codex_dir().join("auth.json"),
            fs::Permissions::from_mode(0o644),
        )
        .expect("set permissive auth mode");

        let output = env.run(&["doctor"]);

        assert!(output.contains("[warn] auth file:"));
        assert!(output.contains("[warn] profiles directory:"));
        assert!(output.contains("[warn] profiles index:"));
        assert!(output.contains("[warn] profiles lock:"));
        assert!(output.contains("doctor --fix"));
    }
}

#[test]
fn ui_doctor_fix_repairs_existing_permissions() {
    let env = TestEnv::new();
    seed_profiles(&env);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(env.profiles_dir(), fs::Permissions::from_mode(0o755))
            .expect("set permissive test mode");
        fs::set_permissions(
            env.profiles_dir().join("profiles.json"),
            fs::Permissions::from_mode(0o644),
        )
        .expect("set permissive index mode");
        fs::set_permissions(
            env.profiles_dir().join(format!("{ALPHA_ID}.json")),
            fs::Permissions::from_mode(0o644),
        )
        .expect("set permissive profile mode");
        fs::set_permissions(
            env.profiles_dir().join("profiles.lock"),
            fs::Permissions::from_mode(0o644),
        )
        .expect("set permissive lock mode");
        fs::set_permissions(
            env.codex_dir().join("auth.json"),
            fs::Permissions::from_mode(0o644),
        )
        .expect("set permissive auth mode");

        env.run(&["doctor", "--fix"]);

        let dir_mode = fs::metadata(env.profiles_dir())
            .expect("profiles dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);

        for file in [
            env.codex_dir().join("auth.json"),
            env.profiles_dir().join("profiles.json"),
            env.profiles_dir().join(format!("{ALPHA_ID}.json")),
            env.profiles_dir().join("profiles.lock"),
        ] {
            let mode = fs::metadata(file)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}

#[test]
fn ui_doctor_fix_json_noop_has_empty_repairs() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["doctor", "--fix", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse doctor json");
    let repairs = json
        .get("repairs")
        .and_then(|value| value.as_array())
        .expect("repairs array");
    assert!(repairs.is_empty());
}

#[test]
fn ui_doctor_fix_does_not_index_stray_json() {
    let env = TestEnv::new();
    fs::create_dir_all(env.profiles_dir()).expect("create profiles dir");
    fs::write(env.profiles_dir().join("notes.json"), "{\"note\":true}").expect("write stray json");

    env.run(&["doctor", "--fix"]);

    let json = read_json_file(&env.profiles_dir().join("profiles.json"));
    let profile_count = json.get("profiles").map_or(0, |value| {
        value
            .as_array()
            .map(|entries| entries.len())
            .or_else(|| value.as_object().map(|entries| entries.len()))
            .unwrap_or(0)
    });
    assert_eq!(profile_count, 0);
    assert!(env.profiles_dir().join("notes.json").is_file());
}

#[test]
fn ui_doctor_fix_json_storage_shape_error() {
    let env = TestEnv::new();
    fs::write(env.codex_dir().join("profiles"), "not-a-directory")
        .expect("write invalid profiles path");

    let output = env.run(&["doctor", "--fix", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse doctor json");
    let checks = json
        .get("checks")
        .and_then(|value| value.as_array())
        .expect("checks array");
    assert!(checks.iter().any(|check| {
        check.get("name") == Some(&serde_json::json!("profiles directory"))
            && check.get("level") == Some(&serde_json::json!("error"))
    }));
    assert!(json
        .get("error")
        .and_then(|value| value.as_str())
        .is_some_and(|error| error.contains("profiles directory exists but is not a directory")));
    let repairs = json
        .get("repairs")
        .and_then(|value| value.as_array())
        .expect("repairs array");
    assert!(repairs.is_empty());
}

#[test]
fn ui_load_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    let output = env.run(&["load", "--label", "beta"]);
    assert!(output.contains("Loaded profile"));
    assert!(output.contains("beta@example.com"));
    assert!(env.read_auth().contains(BETA_ACCOUNT));
}

#[cfg(unix)]
#[test]
fn ui_load_command_restores_private_auth_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    fs::set_permissions(
        env.profiles_dir().join(format!("{BETA_ID}.json")),
        fs::Permissions::from_mode(0o644),
    )
    .expect("set permissive saved profile mode");
    fs::set_permissions(
        env.codex_dir().join("auth.json"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("set permissive auth mode");

    env.run(&["load", "--label", "beta"]);

    let auth_mode = fs::metadata(env.codex_dir().join("auth.json"))
        .expect("auth metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(auth_mode, 0o600);
}

#[test]
fn ui_load_by_id_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    let output = env.run(&["load", "--id", BETA_ID]);
    assert!(output.contains("Loaded profile"));
    assert!(output.contains("beta@example.com"));
    assert!(env.read_auth().contains(BETA_ACCOUNT));
}

#[test]
fn ui_load_with_status_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 1).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["load", "--label", "beta", "--with-status"]);
    assert!(output.contains("Loaded profile"));
    assert!(output.contains(BETA_EMAIL));
    assert_eq!(output.matches(BETA_EMAIL).count(), 1, "{output}");
    assert!(!output.contains(ALPHA_EMAIL));
    assert!(output.contains("80% left"), "{output}");
    assert!(env.read_auth().contains(BETA_ACCOUNT));

    let _ = usage_handle.join();
}

#[test]
fn ui_load_with_status_keeps_load_success_when_usage_fails() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    env.write_config("http://127.0.0.1:1/backend-api");

    let output = env.run_output(&["load", "--label", "beta", "--with-status"]);
    assert!(output.status.success());
    let stdout = ascii_only(String::from_utf8_lossy(&output.stdout).as_ref());
    let stderr = ascii_only(String::from_utf8_lossy(&output.stderr).as_ref());
    assert!(stdout.contains("Loaded profile"));
    assert!(stdout.contains(BETA_EMAIL));
    assert!(stderr.contains("Loaded profile, but status retrieval failed"));
    assert!(stderr.contains("Could not reach usage service"));
    assert!(env.read_auth().contains(BETA_ACCOUNT));
}

#[test]
fn ui_load_by_id_force_skips_unsaved_prompt() {
    let env = TestEnv::new();
    seed_profiles(&env);
    env.write_auth(
        "acct-current",
        "current@example.com",
        "team",
        "token-current",
    );
    let output = env.run(&["load", "--id", BETA_ID, "--force"]);
    assert!(output.contains("Loaded profile"));
    assert!(output.contains("beta@example.com"));
    assert!(env.read_auth().contains(BETA_ACCOUNT));
}

#[test]
fn ui_load_current_profile_marks_current() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    let output = env.run(&["load", "--label", "alpha"]);
    assert!(output.contains("<- active"));
}

#[test]
fn ui_load_label_not_found() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let err = env.run_expect_error(&["load", "--label", "missing"]);
    assert!(err.contains("Label 'missing' was not found"));
}

#[test]
fn ui_load_id_not_found() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let err = env.run_expect_error(&["load", "--id", "missing-id"]);
    assert!(err.contains("Profile id 'missing-id'"));
}

#[test]
fn ui_load_rejects_label_with_id() {
    let env = TestEnv::new();
    let err = env.run_expect_error(&["load", "--label", "alpha", "--id", ALPHA_ID]);
    assert!(err.contains("--label"));
    assert!(err.contains("--id"));
}

#[test]
fn ui_load_rejects_invalid_profile_json() {
    let env = TestEnv::new();
    fs::create_dir_all(env.profiles_dir()).expect("create profiles dir");
    let profile_path = env.profiles_dir().join("broken.json");
    fs::write(&profile_path, "{").expect("write profile");
    env.write_profiles_index(&[("broken", 123)], &[("broken", "broken")], None);
    let err = env.run_expect_error(&["load", "--label", "broken"]);
    assert!(err.contains("Selected profile is invalid") || err.contains("broken.json"));
    assert!(profile_path.is_file());
}

#[test]
fn ui_load_requires_tty() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    let err = env.run_expect_error(&["load"]);
    assert!(err.contains("load selection requires a TTY"));
}

#[test]
fn ui_load_unsaved_profile_requires_prompt() {
    let env = TestEnv::new();
    seed_profiles(&env);
    env.write_auth(
        "acct-current",
        "current@example.com",
        "team",
        "token-current",
    );
    let err = env.run_expect_error(&["load", "--label", "alpha"]);
    assert!(err.contains("Active profile is not saved"));
    assert!(err.contains("--force"));
}

#[test]
fn ui_delete_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["delete", "--label", "beta", "--yes"]);
    assert!(output.contains("Deleted profile"));
    assert!(output.contains("beta@example.com"));
    let profile_path = env.profiles_dir().join(format!("{BETA_ID}.json"));
    assert!(!profile_path.is_file());
}

#[test]
fn ui_delete_by_id_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["delete", "--id", BETA_ID, "--yes"]);
    assert!(output.contains("Deleted profile"));
    assert!(output.contains("beta@example.com"));
    let profile_path = env.profiles_dir().join(format!("{BETA_ID}.json"));
    assert!(!profile_path.is_file());
}

#[test]
fn ui_delete_multiple_ids_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["delete", "--id", BETA_ID, "--id", ALPHA_ID, "--yes"]);
    assert!(output.contains("Deleted 2 profiles"));
    assert!(
        !env.profiles_dir()
            .join(format!("{ALPHA_ID}.json"))
            .is_file()
    );
    assert!(!env.profiles_dir().join(format!("{BETA_ID}.json")).is_file());
}

#[test]
fn ui_delete_current_profile_marks_current() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    let output = env.run(&["delete", "--label", "alpha", "--yes"]);
    assert!(output.contains("<- active"));
}

#[test]
fn ui_delete_requires_tty() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let err = env.run_expect_error(&["delete"]);
    assert!(err.contains("delete selection requires a TTY"));
}

#[test]
fn ui_delete_requires_confirmation() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let err = env.run_expect_error(&["delete", "--label", "beta"]);
    assert!(err.contains("Deletion requires confirmation"));
}

#[test]
fn ui_delete_id_not_found() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let err = env.run_expect_error(&["delete", "--id", "missing-id", "--yes"]);
    assert!(err.contains("Profile id 'missing-id'"));
}

#[test]
fn ui_delete_rejects_label_with_id() {
    let env = TestEnv::new();
    let err = env.run_expect_error(&["delete", "--label", "alpha", "--id", ALPHA_ID]);
    assert!(err.contains("--label"));
    assert!(err.contains("--id"));
}

#[test]
fn ui_delete_no_profiles() {
    let env = TestEnv::new();
    seed_alpha(&env);
    let output = env.run(&["delete", "--yes"]);
    assert!(output.contains("No saved profiles."));
}

#[test]
fn ui_delete_reports_snapshot_errors() {
    let env = TestEnv::new();
    fs::create_dir_all(env.profiles_dir()).expect("create profiles dir");
    fs::write(env.profiles_dir().join("profiles.json"), "{").expect("write invalid index");
    let err = env.run_expect_error(&["delete", "--yes"]);
    assert!(err.contains("Profiles index file"));
    assert!(err.contains("invalid JSON"));
}

#[test]
fn ui_list_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    env.write_profiles_index(
        &[(ALPHA_ID, 200), (BETA_ID, 100)],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    seed_current(&env);
    let output = env.run(&["list"]);
    assert!(output.contains("current@example.com"));
    assert!(output.contains("<- active"));
    assert!(output.contains("Warning: This profile is not saved yet."));
    assert!(output.contains("Run `codex-profiles save` to save this profile."));
    assert!(output.contains("alpha@example.com"));
    assert!(output.contains("beta@example.com"));
    assert_order(&output, "current@example.com", "alpha@example.com");
    assert_order(&output, "alpha@example.com", "beta@example.com");
}

#[test]
fn ui_list_show_id_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let output = env.run(&["list", "--show-id"]);
    assert!(output.contains("[id: alpha@example.com-team]"));
    assert!(output.contains("[id: beta@example.com-team]"));
}

#[test]
fn ui_list_json_exposes_ids_for_scripting() {
    let env = TestEnv::new();
    fs::create_dir_all(env.profiles_dir()).expect("create profiles dir");
    write_profile_tokens(
        &env,
        ALPHA_ID,
        serde_json::json!({
            "account_id": ALPHA_ACCOUNT,
            "id_token": build_id_token(ALPHA_EMAIL, ALPHA_PLAN),
            "access_token": ALPHA_TOKEN,
            "refresh_token": "refresh-alpha"
        }),
    );
    write_profile_tokens(
        &env,
        BETA_ID,
        serde_json::json!({
            "account_id": BETA_ACCOUNT,
            "id_token": build_id_token(BETA_EMAIL, BETA_PLAN),
            "access_token": BETA_TOKEN,
            "refresh_token": "refresh-beta"
        }),
    );
    env.write_profiles_index(
        &[(ALPHA_ID, 200), (BETA_ID, 100)],
        &[(ALPHA_ID, "alpha")],
        None,
    );
    seed_current(&env);

    let output = env.run(&["list", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse list json");
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");

    assert_eq!(profiles.len(), 3);
    assert_eq!(profiles[0].get("id").unwrap(), &serde_json::Value::Null);
    assert_eq!(
        profiles[0].get("is_current").unwrap(),
        &serde_json::json!(true)
    );
    assert_eq!(
        profiles[0].get("is_saved").unwrap(),
        &serde_json::json!(false)
    );
    assert_eq!(
        profiles[0].get("email").unwrap(),
        &serde_json::json!("current@example.com")
    );

    assert_eq!(profiles[1].get("id").unwrap(), &serde_json::json!(ALPHA_ID));
    assert_eq!(
        profiles[1].get("label").unwrap(),
        &serde_json::json!("alpha")
    );
    assert_eq!(
        profiles[1].get("is_saved").unwrap(),
        &serde_json::json!(true)
    );

    assert_eq!(profiles[2].get("id").unwrap(), &serde_json::json!(BETA_ID));
    assert_eq!(profiles[2].get("label").unwrap(), &serde_json::Value::Null);
}

#[test]
fn ui_list_json_empty_profiles() {
    let env = TestEnv::new();
    let output = env.run(&["list", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse list json");
    assert_eq!(json, serde_json::json!({ "profiles": [] }));
}

#[test]
fn ui_list_json_accepts_show_id_without_changing_output() {
    let env = TestEnv::new();
    seed_profiles(&env);

    let plain_json = env.run(&["list", "--json"]);
    let show_id_json = env.run(&["list", "--json", "--show-id"]);

    let plain: serde_json::Value =
        serde_json::from_str(&plain_json).expect("parse plain list json");
    let with_show_id: serde_json::Value =
        serde_json::from_str(&show_id_json).expect("parse show-id list json");

    assert_eq!(with_show_id, plain);
}

#[test]
fn ui_label_set_help_shows_exactly_one_selector() {
    let env = TestEnv::new();
    let output = env.run_output(&["label", "set", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);

    assert!(
        help.contains(
            "Usage: codex-profiles label set [OPTIONS] --to <label> (--label <label> | --id <profile-id>)"
        )
    );
    assert!(help.contains("--label <label>"));
    assert!(help.contains("--id <profile-id>"));
    assert!(help.contains("--to <label>"));
}

#[test]
fn ui_label_set_missing_selector_has_clear_error() {
    let env = TestEnv::new();
    let err = env.run_expect_error(&["label", "set", "--to", "work"]);

    assert!(err.contains("exactly one of `--label <label>` or `--id <profile-id>` is required"));
    assert!(err.contains(
        "Usage: codex-profiles label set [OPTIONS] --to <label> (--label <label> | --id <profile-id>)"
    ));
}

#[test]
fn ui_label_clear_missing_selector_has_clear_error() {
    let env = TestEnv::new();
    let err = env.run_expect_error(&["label", "clear"]);

    assert!(err.contains("exactly one of `--label <label>` or `--id <profile-id>` is required"));
    assert!(err.contains(
        "Usage: codex-profiles label clear [OPTIONS] (--label <label> | --id <profile-id>)"
    ));
}

#[test]
fn ui_list_free_plan() {
    let env = TestEnv::new();
    seed_free(&env);
    env.run(&["save", "--label", "free"]);
    let output = env.run(&["list"]);
    assert!(output.contains(FREE_EMAIL));
    assert!(!output.contains("You need a ChatGPT subscription to use Codex CLI"));
    assert!(!output.contains("Data not available"));
}

#[test]
fn ui_list_unsaved_free_profile_shows_warning() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);
    seed_free(&env);

    let output = env.run(&["list"]);
    assert!(output.contains(FREE_EMAIL));
    assert!(output.contains("Warning: This profile is not saved yet."));
    assert!(output.contains("Run `codex-profiles save` to save this profile."));
}

#[test]
fn ui_list_does_not_sync_current_profile() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);
    seed_alpha_with_token(&env, "token-alpha-rotated");
    env.run(&["list"]);
    let profile_path = env.profiles_dir().join(format!("{ALPHA_ID}.json"));
    let contents = fs::read_to_string(profile_path).expect("read profile");
    assert!(!contents.contains("token-alpha-rotated"));
    assert!(contents.contains(ALPHA_TOKEN));
}

#[test]
fn ui_profiles_index_does_not_store_last_used() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let index_path = env.profiles_dir().join("profiles.json");
    let index = fs::read_to_string(index_path).expect("read profiles.json");
    let json: serde_json::Value = serde_json::from_str(&index).expect("parse profiles.json");
    let profiles = json.get("profiles").expect("profiles map");
    let alpha_last_used = profiles
        .get(ALPHA_ID)
        .and_then(|entry| entry.get("last_used"))
        .and_then(|value| value.as_u64());
    let beta_last_used = profiles
        .get(BETA_ID)
        .and_then(|entry| entry.get("last_used"))
        .and_then(|value| value.as_u64());
    assert!(alpha_last_used.is_none());
    assert!(beta_last_used.is_none());
}

#[test]
fn ui_save_adds_missing_profiles_to_index() {
    let env = TestEnv::new();
    seed_alpha(&env);
    let profile_path = env.profiles_dir().join(format!("{ALPHA_ID}.json"));
    fs::create_dir_all(env.profiles_dir()).expect("create profiles dir");
    fs::copy(env.codex_dir().join("auth.json"), &profile_path).expect("seed profile file");
    env.write_profiles_index(&[], &[], None);
    seed_beta(&env);
    let output = env.run(&["save", "--label", "beta"]);
    assert!(output.contains("Saved profile"));
    let contents =
        fs::read_to_string(env.profiles_dir().join("profiles.json")).expect("read profiles.json");
    let json: serde_json::Value = serde_json::from_str(&contents).expect("parse profiles.json");
    let profiles = json.get("profiles").expect("profiles map");
    assert!(profiles.get(ALPHA_ID).is_some());
    assert!(profiles.get(BETA_ID).is_some());
}

#[test]
fn ui_status_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    assert_status_output(&env, &["status"], &["alpha@example.com"]);
    let output = env.run(&["status"]);
    assert!(output.contains("alpha@example.com"));
    assert!(!output.contains("beta@example.com"));
    assert!(!output.contains("<- active"));
}

#[test]
fn ui_status_json_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--json"]);
    let profile: serde_json::Value = serde_json::from_str(&output).expect("parse status json");
    assert_eq!(profile.get("id").unwrap(), &serde_json::json!(ALPHA_ID));
    assert_eq!(profile.get("label").unwrap(), &serde_json::json!("alpha"));
    assert_eq!(
        profile.get("email").unwrap(),
        &serde_json::json!(ALPHA_EMAIL)
    );
    assert_eq!(profile.get("is_current").unwrap(), &serde_json::json!(true));
    assert_eq!(profile.get("is_saved").unwrap(), &serde_json::json!(true));
    assert!(profile.get("details").is_none());
    assert_eq!(profile.get("error").unwrap(), &serde_json::Value::Null);
    let usage = profile.get("usage").expect("usage");
    assert_eq!(usage.get("state").unwrap(), &serde_json::json!("ok"));
    let buckets = usage
        .get("buckets")
        .and_then(|value| value.as_array())
        .expect("usage buckets");
    assert_eq!(buckets.len(), 1);
    assert_eq!(buckets[0].get("id").unwrap(), &serde_json::json!("codex"));
    assert_eq!(
        buckets[0].get("label").unwrap(),
        &serde_json::json!("codex")
    );
    assert_eq!(
        buckets[0]
            .get("five_hour")
            .and_then(|value| value.get("left_percent"))
            .unwrap(),
        &serde_json::json!(80)
    );
    assert_eq!(
        buckets[0]
            .get("five_hour")
            .and_then(|value| value.get("reset_at"))
            .unwrap(),
        &serde_json::json!(2000000000)
    );
    assert_eq!(buckets[0].get("weekly").unwrap(), &serde_json::Value::Null);

    let _ = usage_handle.join();
}

#[test]
fn ui_status_json_empty_profile() {
    let env = TestEnv::new();
    let output = env.run(&["status", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse status json");
    assert_eq!(json, serde_json::Value::Null);
}

#[test]
fn ui_status_json_unsaved_current_profile() {
    let env = TestEnv::new();
    seed_current(&env);
    let output = env.run(&["status", "--json"]);
    let profile: serde_json::Value = serde_json::from_str(&output).expect("parse status json");
    assert_eq!(profile.get("id").unwrap(), &serde_json::Value::Null);
    assert_eq!(
        profile.get("email").unwrap(),
        &serde_json::json!("current@example.com")
    );
    assert_eq!(profile.get("is_current").unwrap(), &serde_json::json!(true));
    assert_eq!(profile.get("is_saved").unwrap(), &serde_json::json!(false));
    let warnings = profile
        .get("warnings")
        .and_then(|value| value.as_array())
        .expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|value| value == "Warning: This profile is not saved yet.")
    );
    assert!(
        warnings
            .iter()
            .any(|value| value == "Run `codex-profiles save` to save this profile.")
    );
}

#[test]
fn ui_status_label_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--label", "beta"]);
    assert!(output.contains(BETA_EMAIL));
    assert!(!output.contains(ALPHA_EMAIL));
    assert!(!output.contains("<- active"));

    let _ = usage_handle.join();
}

#[test]
fn ui_status_id_json_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--id", BETA_ID, "--json"]);
    let profile: serde_json::Value = serde_json::from_str(&output).expect("parse status json");
    assert_eq!(profile.get("id").unwrap(), &serde_json::json!(BETA_ID));
    assert_eq!(profile.get("label").unwrap(), &serde_json::json!("beta"));
    assert_eq!(
        profile.get("email").unwrap(),
        &serde_json::json!(BETA_EMAIL)
    );
    assert_eq!(
        profile.get("is_current").unwrap(),
        &serde_json::json!(false)
    );
    assert_eq!(profile.get("is_saved").unwrap(), &serde_json::json!(true));

    let _ = usage_handle.join();
}

#[test]
fn ui_status_label_json_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--label", "beta", "--json"]);
    let profile: serde_json::Value = serde_json::from_str(&output).expect("parse status json");
    assert_eq!(profile.get("id").unwrap(), &serde_json::json!(BETA_ID));
    assert_eq!(profile.get("label").unwrap(), &serde_json::json!("beta"));

    let _ = usage_handle.join();
}

#[test]
fn ui_status_selector_json_empty_profiles() {
    let env = TestEnv::new();
    let output = env.run(&["status", "--label", "beta", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse status json");
    assert_eq!(json, serde_json::Value::Null);
}

#[test]
fn ui_status_rejects_all_with_label() {
    let env = TestEnv::new();
    let err = env.run_expect_error(&["status", "--all", "--label", "beta"]);
    assert!(err.contains("--all"));
    assert!(err.contains("--label"));
}

#[test]
fn ui_status_rejects_all_with_id() {
    let env = TestEnv::new();
    let err = env.run_expect_error(&["status", "--all", "--id", BETA_ID]);
    assert!(err.contains("--all"));
    assert!(err.contains("--id"));
}

#[test]
fn ui_status_rejects_label_with_id() {
    let env = TestEnv::new();
    let err = env.run_expect_error(&["status", "--label", "beta", "--id", BETA_ID]);
    assert!(err.contains("--label"));
    assert!(err.contains("--id"));
}

#[test]
fn ui_status_label_not_found() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let err = env.run_expect_error(&["status", "--label", "missing"]);
    assert!(err.contains("Label 'missing'"));
}

#[test]
fn ui_status_id_not_found() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let err = env.run_expect_error(&["status", "--id", "missing-id"]);
    assert!(err.contains("Profile id 'missing-id'"));
}

#[test]
fn ui_status_all_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    env.write_profiles_index(
        &[(ALPHA_ID, 200), (BETA_ID, 100)],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    seed_alpha(&env);
    assert_status_output(
        &env,
        &["status", "--all"],
        &["alpha@example.com", "beta@example.com"],
    );
    let output = env.run(&["status", "--all"]);
    assert!(output.contains("<- active"));
    assert_order(&output, "alpha@example.com", "beta@example.com");
}

#[test]
fn ui_status_all_rejects_removed_show_errors_flag() {
    let env = TestEnv::new();
    let err = env.run_expect_error(&["status", "--all", "--show-errors"]);
    assert!(err.contains("unexpected argument '--show-errors'"));
}

#[test]
fn ui_status_all_json_command() {
    let env = TestEnv::new();
    seed_profiles(&env);
    env.write_profiles_index(
        &[(ALPHA_ID, 200), (BETA_ID, 100)],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    seed_alpha(&env);
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse status all json");
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert_eq!(profiles.len(), 2);
    assert_eq!(profiles[0].get("id").unwrap(), &serde_json::json!(ALPHA_ID));
    assert_eq!(profiles[1].get("id").unwrap(), &serde_json::json!(BETA_ID));
    assert!(json.get("hidden_api_profiles").is_none());
    assert!(json.get("hidden_error_profiles").is_none());
    assert_eq!(profiles[0].get("details"), None);
    assert_eq!(
        profiles[0]
            .get("usage")
            .and_then(|value| value.get("state"))
            .unwrap(),
        &serde_json::json!("ok")
    );

    let _ = usage_handle.join();
}

#[test]
fn ui_status_json_402_exposes_structured_usage_unavailable() {
    let env = TestEnv::new();
    let profile_id = "mail1@example.com-team";
    write_profile_tokens(
        &env,
        profile_id,
        serde_json::json!({
            "account_id": "acct-mail-1",
            "access_token": "token-mail-1",
            "refresh_token": "refresh-mail-1"
        }),
    );
    env.write_profiles_index(
        &[(profile_id, 10)],
        &[(profile_id, "mail1")],
        Some(profile_id),
    );
    seed_current(&env);

    let usage_402_body = r#"{"detail":{"code":"deactivated_workspace"}}"#;
    let usage_402_resp = format!(
        "HTTP/1.1 402 Payment Required\r\nContent-Type: application/json\r\nCF-Ray: ray-402\r\nx-request-id: req-402\r\nContent-Length: {}\r\n\r\n{}",
        usage_402_body.len(),
        usage_402_body
    );
    let (usage_addr, usage_handle) = start_response_server(vec![usage_402_resp], 4).expect("usage");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--json"]);
    let profile: serde_json::Value = serde_json::from_str(&output).expect("parse status json");
    assert!(profile.get("details").is_none());
    let error = profile.get("error").expect("error object");
    let summary = error
        .get("summary")
        .and_then(|summary| summary.get("message"))
        .and_then(serde_json::Value::as_str)
        .expect("error summary message");
    assert!(summary.starts_with("Usage error: unexpected status 402 Payment Required"));
    assert!(summary.contains("/backend-api/wham/usage"));
    assert_eq!(
        error
            .get("summary")
            .and_then(|summary| summary.get("response"))
            .unwrap(),
        &serde_json::json!({"detail": {"code": "deactivated_workspace"}})
    );
    assert_eq!(error.get("status_code").unwrap(), &serde_json::json!(402));
    assert!(error.get("detail").is_none() || error.get("detail") == Some(&serde_json::Value::Null));
    let usage = profile.get("usage").expect("usage");
    assert_eq!(usage, &serde_json::json!({ "state": "error" }));

    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_json_includes_api_profiles() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    seed_api_profile(&env, "api-key-hidden", "api-key-sk-proj-hidden1234567890");
    env.write_profiles_index(
        &[(ALPHA_ID, 300), (BETA_ID, 200), ("api-key-hidden", 100)],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse status all json");
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert_eq!(profiles.len(), 3);
    let api_profile = profiles
        .iter()
        .find(|profile| profile.get("id") == Some(&serde_json::json!("api-key-hidden")))
        .expect("api profile");
    assert_eq!(
        api_profile.get("is_api_key").unwrap(),
        &serde_json::json!(true)
    );
    let usage = api_profile.get("usage").expect("api usage");
    assert_eq!(
        usage.get("state").unwrap(),
        &serde_json::json!("unavailable")
    );
    assert!(
        usage
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .contains("Usage unavailable for API key")
    );
    assert!(api_profile.get("error").is_none() || api_profile.get("error").unwrap().is_null());

    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_json_empty_profiles() {
    let env = TestEnv::new();
    let output = env.run(&["status", "--all", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse status all json");
    assert_eq!(
        json,
        serde_json::json!({
            "profiles": []
        })
    );
}

#[test]
fn ui_status_all_json_includes_errored_profiles_by_default() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    seed_errored_profile(&env, "gamma@example.com-team");
    env.write_profiles_index(
        &[
            (ALPHA_ID, 300),
            (BETA_ID, 200),
            ("gamma@example.com-team", 100),
        ],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse status all json");
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert_eq!(profiles.len(), 3);
    assert!(
        profiles
            .iter()
            .any(|profile| profile.get("error").is_some()
                && !profile.get("error").unwrap().is_null())
    );

    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_json_includes_errored_profiles() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    seed_errored_profile(&env, "gamma@example.com-team");
    env.write_profiles_index(
        &[
            (ALPHA_ID, 300),
            (BETA_ID, 200),
            ("gamma@example.com-team", 100),
        ],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse status all json");
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert_eq!(profiles.len(), 3);
    assert!(
        profiles
            .iter()
            .any(|profile| profile.get("error").is_some()
                && !profile.get("error").unwrap().is_null())
    );

    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_json_402_exposes_structured_usage_error() {
    let env = TestEnv::new();
    let profile_id = "mail1@example.com-team";
    write_profile_tokens(
        &env,
        profile_id,
        serde_json::json!({
            "account_id": "acct-mail-1",
            "access_token": "token-mail-1",
            "refresh_token": "refresh-mail-1"
        }),
    );
    env.write_profiles_index(
        &[(profile_id, 10)],
        &[(profile_id, "mail1")],
        Some(profile_id),
    );
    seed_current(&env);

    let usage_402_body = r#"{"detail":{"code":"deactivated_workspace"}}"#;
    let usage_402_resp = format!(
        "HTTP/1.1 402 Payment Required\r\nContent-Type: application/json\r\nCF-Ray: ray-402\r\nx-request-id: req-402\r\nContent-Length: {}\r\n\r\n{}",
        usage_402_body.len(),
        usage_402_body
    );
    let (usage_addr, usage_handle) = start_response_server(vec![usage_402_resp], 4).expect("usage");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all", "--json"]);
    let payload: serde_json::Value = serde_json::from_str(&output).expect("parse status all json");
    let profiles = payload
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert!(!profiles.is_empty());
    let profile = profiles
        .iter()
        .find(|item| {
            item.get("error").and_then(|error| error.get("status_code"))
                == Some(&serde_json::json!(402))
        })
        .expect("profile with 402 status error");

    let error = profile.get("error").expect("error object");
    let message = error
        .get("summary")
        .and_then(|summary| summary.get("message"))
        .and_then(serde_json::Value::as_str)
        .expect("error summary message");
    assert!(message.starts_with("Usage error: unexpected status 402 Payment Required"));
    assert!(message.contains("/backend-api/wham/usage"));
    assert_eq!(
        error
            .get("summary")
            .and_then(|summary| summary.get("response"))
            .unwrap(),
        &serde_json::json!({"detail": {"code": "deactivated_workspace"}})
    );
    assert_eq!(error.get("status_code").unwrap(), &serde_json::json!(402));
    assert!(error.get("detail").is_none() || error.get("detail") == Some(&serde_json::Value::Null));
    assert_eq!(
        profile.get("usage").unwrap(),
        &serde_json::json!({ "state": "error" })
    );

    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_layout_snapshot_spacing() {
    let env = TestEnv::new();
    seed_profiles(&env);
    env.write_profiles_index(
        &[(ALPHA_ID, 200), (BETA_ID, 100)],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    seed_alpha(&env);
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000},"secondary_window":{"used_percent":50,"limit_window_seconds":604800,"reset_at":2000600000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all"]);
    assert_order(&output, ALPHA_EMAIL, BETA_EMAIL);
    assert_profile_block_layout(&output, ALPHA_EMAIL, Some(BETA_EMAIL));
    assert_profile_block_layout(&output, BETA_EMAIL, None);
    assert!(output.contains("<- active"));

    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_renders_grouped_multi_bucket_windows() {
    let env = TestEnv::new();
    seed_profiles(&env);
    let usage_body = r#"{
        "rate_limit":{
            "primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000},
            "secondary_window":{"used_percent":50,"limit_window_seconds":604800,"reset_at":2000600000}
        },
        "additional_rate_limits":[
            {
                "limit_name":"codex_other",
                "metered_feature":"codex_other",
                "rate_limit":{
                    "primary_window":{"used_percent":40,"limit_window_seconds":3600,"reset_at":2000001200}
                }
            }
        ]
    }"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all"]);
    assert!(output.contains("codex"));
    assert!(output.contains("5 hour:"));
    assert!(output.contains("Weekly:"));
    assert!(output.contains("codex_other"));
    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_unsaved_free_profile_shows_warning() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);
    seed_free(&env);

    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all"]);
    assert!(output.contains(FREE_EMAIL));
    assert!(output.contains("Warning: This profile is not saved yet."));
    assert!(output.contains("Run `codex-profiles save` to save this profile."));
    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_no_usage() {
    let env = TestEnv::new();
    seed_profiles(&env);
    env.write_profiles_index(
        &[(ALPHA_ID, 200), (BETA_ID, 100)],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    seed_alpha(&env);
    env.write_config("http://127.0.0.1:1/backend-api");
    let output = env.run(&["status", "--all"]);
    assert!(output.contains("alpha@example.com"));
    assert!(output.contains("beta@example.com"));
    assert!(output.contains("Error:"));
}

#[test]
fn ui_status_all_includes_api_profiles_by_default() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    seed_api_profile(&env, "api-key-hidden", "api-key-sk-proj-hidden1234567890");
    env.write_profiles_index(
        &[(ALPHA_ID, 300), (BETA_ID, 200), ("api-key-hidden", 100)],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all"]);
    assert!(output.contains("alpha@example.com"));
    assert!(output.contains("beta@example.com"));
    assert!(output.contains("Usage unavailable for API key"));
    assert!(!output.contains("API profiles hidden"));
    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_includes_errored_profiles_by_default() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    seed_errored_profile(&env, "gamma@example.com-team");
    env.write_profiles_index(
        &[
            (ALPHA_ID, 300),
            (BETA_ID, 200),
            ("gamma@example.com-team", 100),
        ],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all"]);
    assert!(output.contains("alpha@example.com"));
    assert!(output.contains("beta@example.com"));
    assert!(output.contains("Error: Profile is missing email or plan information."));
    assert!(!output.contains("errored profiles hidden"));
    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_includes_errored_profiles() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_alpha(&env);
    seed_errored_profile(&env, "gamma@example.com-team");
    env.write_profiles_index(
        &[
            (ALPHA_ID, 300),
            (BETA_ID, 200),
            ("gamma@example.com-team", 100),
        ],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all"]);
    assert!(output.contains("alpha@example.com"));
    assert!(output.contains("beta@example.com"));
    assert!(output.contains("Error: Profile is missing email or plan information."));
    assert!(!output.contains("errored profiles hidden"));
    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_includes_current_api_profile() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_api_profile(&env, "api-key-current", "api-key-sk-proj-current1234567890");
    seed_api_profile(&env, "api-key-hidden", "api-key-sk-proj-hidden1234567890");
    env.write_profiles_index(
        &[
            (ALPHA_ID, 300),
            (BETA_ID, 200),
            ("api-key-current", 150),
            ("api-key-hidden", 100),
        ],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    write_auth_tokens(
        &env,
        serde_json::json!({
            "account_id": "api-key-sk-proj-current1234567890",
        }),
    );
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all"]);
    assert!(output.contains("alpha@example.com"));
    assert!(output.contains("beta@example.com"));
    assert!(output.contains("Usage unavailable for API key"));
    assert!(!output.contains("API profiles hidden"));
    let _ = usage_handle.join();
}

#[test]
fn ui_status_all_json_includes_current_api_profile() {
    let env = TestEnv::new();
    seed_profiles(&env);
    seed_api_profile(&env, "api-key-current", "api-key-sk-proj-current1234567890");
    seed_api_profile(&env, "api-key-hidden", "api-key-sk-proj-hidden1234567890");
    env.write_profiles_index(
        &[
            (ALPHA_ID, 300),
            (BETA_ID, 200),
            ("api-key-current", 150),
            ("api-key-hidden", 100),
        ],
        &[(ALPHA_ID, "alpha"), (BETA_ID, "beta")],
        None,
    );
    write_auth_tokens(
        &env,
        serde_json::json!({
            "account_id": "api-key-sk-proj-current1234567890",
        }),
    );
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 6).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse status all json");
    let profiles = json
        .get("profiles")
        .and_then(|value| value.as_array())
        .expect("profiles array");
    assert_eq!(profiles.len(), 4);
    assert_eq!(
        profiles
            .iter()
            .filter(|profile| profile.get("is_api_key") == Some(&serde_json::json!(true)))
            .count(),
        2
    );

    let _ = usage_handle.join();
}

#[test]
fn ui_list_preserves_invalid_profiles() {
    let env = TestEnv::new();
    fs::create_dir_all(env.profiles_dir()).expect("create profiles dir");
    let bad_profile = env.profiles_dir().join("bad.json");
    fs::write(&bad_profile, "{").expect("write bad profile");
    env.write_profiles_index(&[("bad", 123)], &[("bad", "bad")], None);

    env.run(&["list"]);

    assert!(bad_profile.is_file());
    let index =
        fs::read_to_string(env.profiles_dir().join("profiles.json")).expect("read profiles.json");
    let json: serde_json::Value = serde_json::from_str(&index).expect("parse profiles.json");
    let profiles = json.get("profiles").expect("profiles map");
    assert!(profiles.get("bad").is_some());
}

#[test]
fn ui_status_refreshes_and_mutates_profile_on_usage_401() {
    let env = TestEnv::new();
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let usage_ok = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        usage_body.len(),
        usage_body
    );
    let usage_unauthorized = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_string();
    let refresh_id_token = build_id_token(ALPHA_EMAIL, ALPHA_PLAN);
    let refresh_body = format!(
        "{{\"id_token\":\"{refresh_id_token}\",\"access_token\":\"new-access\",\"refresh_token\":\"new-refresh\"}}"
    );
    let (usage_addr, usage_handle) =
        start_response_server(vec![usage_unauthorized, usage_ok], 4).expect("usage server");
    let (refresh_addr, refresh_handle) =
        start_usage_server(Box::leak(refresh_body.into_boxed_str()), 2).expect("refresh server");

    env.write_config(&format!("http://{usage_addr}/backend-api"));
    env.write_auth_with_refresh(
        ALPHA_ACCOUNT,
        ALPHA_EMAIL,
        ALPHA_PLAN,
        "old-access",
        "refresh-old",
    );
    env.run(&["save", "--label", "alpha"]);

    let refresh_url = format!("http://{refresh_addr}/token");
    let output = env.run_with_env(
        &["status"],
        &[("CODEX_REFRESH_TOKEN_URL_OVERRIDE", refresh_url.as_str())],
    );

    let auth_contents = env.read_auth();
    assert!(output.contains("80% left"));
    assert!(auth_contents.contains("new-access"));
    assert!(auth_contents.contains("new-refresh"));
    assert!(!auth_contents.contains("old-access"));
    let profile_path = env.profiles_dir().join(format!("{ALPHA_ID}.json"));
    let profile_contents = fs::read_to_string(profile_path).expect("read profile");
    assert!(profile_contents.contains("new-access"));
    assert!(profile_contents.contains("new-refresh"));
    assert!(!profile_contents.contains("old-access"));

    let _ = usage_handle.join();
    let _ = refresh_handle.join();
}

#[test]
fn ui_status_all_reports_invalid_base_url() {
    let env = TestEnv::new();
    seed_profiles(&env);
    env.write_config("http://example.com");

    let output = env.run(&["status", "--all"]);

    assert!(output.contains("Unsupported chatgpt_base_url"));
}

#[test]
fn ui_status_reports_snapshot_errors() {
    let env = TestEnv::new();
    fs::write(env.profiles_dir(), "not-a-directory").expect("create invalid profiles path");

    let err = env.run_expect_error(&["status"]);

    assert!(err.contains("not a directory"));
}

#[test]
fn ui_status_all_uses_usage_path_when_id_token_missing() {
    let env = TestEnv::new();
    let profile_id = "mail1@example.com-team";
    write_profile_tokens(
        &env,
        profile_id,
        serde_json::json!({
            "account_id": "acct-mail-1",
            "access_token": "token-mail-1",
            "refresh_token": "refresh-mail-1"
        }),
    );
    env.write_profiles_index(
        &[(profile_id, 10)],
        &[(profile_id, "mail1")],
        Some(profile_id),
    );
    seed_current(&env);

    let usage_402_body = r#"{"detail":{"code":"deactivated_workspace"}}"#;
    let usage_402_resp = format!(
        "HTTP/1.1 402 Payment Required\r\nContent-Type: application/json\r\nCF-Ray: ray-402\r\nx-request-id: req-402\r\nContent-Length: {}\r\n\r\n{}",
        usage_402_body.len(),
        usage_402_body
    );
    let (usage_addr, usage_handle) = start_response_server(vec![usage_402_resp], 4).expect("usage");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let output = env.run(&["status", "--all"]);
    assert!(output.contains("mail1"));
    assert!(output.contains("deactivated_workspace (unexpected status 402 Payment Required)"));
    assert!(!output.contains("deactivated_workspace\n  unexpected status 402 Payment Required"));
    assert!(output.contains("\n   URL: http://"));
    assert!(output.contains("\n   CF-Ray: ray-402"));
    assert!(output.contains("\n   Request ID: req-402"));
    assert!(output.contains("/backend-api/wham/usage"));
    assert!(!output.contains("{\"detail\":{\"code\":\"deactivated_workspace\"}}"));
    assert!(!output.contains("Auth is incomplete. Run `codex login`."));

    let _ = usage_handle.join();
}

#[test]
fn ui_status_api_key_error_message_is_standardized() {
    let env = TestEnv::new();
    write_auth_tokens(
        &env,
        serde_json::json!({
            "account_id": "api-key-sk-proj-abcdef1234567890",
            "refresh_token": ""
        }),
    );

    let output = env.run(&["status"]);
    assert!(output.contains("Error: Usage unavailable for API key"));
    assert!(
        output.contains("Rate-limit usage data is only available for ChatGPT account profiles.")
    );
}

// ---------------------------------------------------------------------------
// --json output tests for all mutating commands
// ---------------------------------------------------------------------------

fn parse_json(output: &str) -> serde_json::Value {
    serde_json::from_str(output.trim())
        .unwrap_or_else(|e| panic!("Expected valid JSON output, got: {output:?}\nError: {e}"))
}

#[test]
fn json_save_returns_success_shape() {
    let env = TestEnv::new();
    seed_current(&env);

    let raw = env.run(&["save", "--label", "work", "--json"]);
    let v = parse_json(&raw);

    assert_eq!(v["command"], "save", "command field");
    assert_eq!(v["success"], true, "success field");
    let profile = &v["profile"];
    assert!(profile["id"].is_string(), "profile.id is string");
    assert_eq!(profile["label"], "work", "profile.label");
    assert!(profile.get("default").is_none(), "profile.default removed");
}

#[test]
fn json_load_returns_success_shape() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);

    let raw = env.run(&["load", "--label", "alpha", "--json"]);
    let v = parse_json(&raw);

    assert_eq!(v["command"], "load");
    assert_eq!(v["success"], true);
    let profile = &v["profile"];
    assert!(profile["id"].is_string());
    assert_eq!(profile["label"], "alpha");
    assert!(profile.get("default").is_none());
}

#[test]
fn json_load_with_status_returns_success_shape() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);
    let usage_body = r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#;
    let (usage_addr, usage_handle) = start_usage_server(usage_body, 1).expect("usage server");
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let raw = env.run(&["load", "--label", "alpha", "--with-status", "--json"]);
    let v = parse_json(&raw);

    assert_eq!(v["command"], "load");
    assert_eq!(v["success"], true);
    let profile = &v["profile"];
    assert_eq!(profile["label"], "alpha");
    assert_eq!(profile["status"]["id"], ALPHA_ID);
    assert_eq!(profile["status"]["usage"]["state"], "ok");

    let _ = usage_handle.join();
}

#[test]
fn json_delete_returns_success_shape() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);

    let raw = env.run(&["delete", "--label", "alpha", "--yes", "--json"]);
    let v = parse_json(&raw);

    assert_eq!(v["command"], "delete");
    assert_eq!(v["success"], true);
    let profile = &v["profile"];
    assert!(profile["count"].is_number(), "profile.count is number");
    let deleted = profile["deleted"].as_array().expect("deleted is array");
    assert!(!deleted.is_empty(), "at least one profile deleted");
    assert!(deleted[0]["id"].is_string(), "deleted[0].id is string");
}

#[test]
fn json_label_set_returns_success_shape() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);

    let raw = env.run(&[
        "label", "set", "--id", ALPHA_ID, "--to", "newalpha", "--json",
    ]);
    let v = parse_json(&raw);

    assert_eq!(v["command"], "label set");
    assert_eq!(v["success"], true);
    let profile = &v["profile"];
    assert!(profile["id"].is_string());
    assert_eq!(profile["label"], "newalpha");
    assert!(profile.get("default").is_none());
}

#[test]
fn json_label_clear_returns_success_shape() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);

    let raw = env.run(&["label", "clear", "--id", ALPHA_ID, "--json"]);
    let v = parse_json(&raw);

    assert_eq!(v["command"], "label clear");
    assert_eq!(v["success"], true);
    let profile = &v["profile"];
    assert!(profile["id"].is_string());
    assert!(profile["label"].is_null() || profile["label"] == "");
    assert!(profile.get("default").is_none());
}

#[test]
fn json_label_rename_returns_success_shape() {
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]);

    let raw = env.run(&[
        "label", "rename", "--label", "alpha", "--to", "renamed", "--json",
    ]);
    let v = parse_json(&raw);

    assert_eq!(v["command"], "label rename");
    assert_eq!(v["success"], true);
    let profile = &v["profile"];
    assert!(profile["id"].is_string());
    assert_eq!(profile["label"], "renamed");
    assert!(profile.get("default").is_none());
}

#[test]
fn json_export_returns_success_shape() {
    let env = TestEnv::new();
    seed_alpha(&env);
    seed_beta(&env);
    let out_path = env.home_path().join("exported.json");

    let raw = env.run(&["export", "--output", out_path.to_str().unwrap(), "--json"]);
    let v = parse_json(&raw);

    assert_eq!(v["command"], "export");
    assert_eq!(v["success"], true);
    let profile = &v["profile"];
    assert!(profile["count"].is_number(), "profile.count is number");
    assert!(profile["path"].is_string(), "profile.path is string");
    assert!(out_path.exists(), "export file should exist on disk");
}

#[test]
fn json_import_returns_success_shape() {
    // Export from one env, import into a fresh one.
    let export_env = TestEnv::new();
    seed_alpha(&export_env);
    seed_beta(&export_env);
    let bundle = export_env.home_path().join("bundle.json");
    export_env.run(&["export", "--output", bundle.to_str().unwrap()]);

    let import_env = TestEnv::new();
    let raw = import_env.run(&["import", "--input", bundle.to_str().unwrap(), "--json"]);
    let v = parse_json(&raw);

    assert_eq!(v["command"], "import");
    assert_eq!(v["success"], true);
    let profile = &v["profile"];
    assert!(profile["count"].is_number());
    assert!(profile["profiles"].is_array());
    let imported = profile["profiles"].as_array().expect("profiles array");
    assert!(imported.iter().all(|entry| entry.get("default").is_none()));
}

#[test]
fn json_mutating_command_error_exits_nonzero_no_json_on_stdout() {
    // delete a label that doesn't exist → error path; stdout must be empty / non-JSON.
    let env = TestEnv::new();
    seed_alpha(&env);
    env.run(&["save", "--label", "alpha"]); // ensure there IS a profile store

    let output = std::process::Command::new(&env.bin_path)
        .args(["delete", "--label", "nonexistent", "--yes", "--json"])
        .env("HOME", env.home_path())
        .env("CODEX_PROFILES_HOME", env.home_path())
        .env("CODEX_PROFILES_COMMAND", "codex-profiles")
        .env("CODEX_PROFILES_SKIP_UPDATE", "1")
        .env("NO_COLOR", "1")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to spawn binary");

    assert!(
        !output.status.success(),
        "expected non-zero exit for missing label"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // stdout must NOT contain valid JSON (errors go to stderr)
    assert!(
        serde_json::from_str::<serde_json::Value>(stdout.trim()).is_err()
            || stdout.trim().is_empty(),
        "stdout should be empty or non-JSON on error, got: {stdout:?}"
    );
}

// ---------------------------------------------------------------------------
// Backward compatibility tests
// ---------------------------------------------------------------------------

/// A v0.1.0-format profiles.json (no `version`, has `active_profile_id`,
/// profile entries have `last_used`/`added_at`, embedded `update_cache`)
/// must be read without error and the profiles it contains must be visible.
#[test]
fn compat_v01_profiles_index_migrates_on_read() {
    let env = TestEnv::new();

    // Write the profile token file that the index refers to.
    let id = "legacy-v01-id";
    write_profile_tokens(
        &env,
        id,
        serde_json::json!({
            "account_id": "acct-legacy",
            "id_token": "tok-legacy",
            "access_token": "tok-legacy-access"
        }),
    );

    // Write a raw v0.1.0-style profiles.json with no `version` field.
    let index_path = env.profiles_dir().join("profiles.json");
    let v01_index = serde_json::json!({
        "active_profile_id": id,
        "profiles": {
            id: {
                "last_used": 1_700_000_000u64,
                "added_at": 1u64,
                "label": "legacy-work"
            }
        },
        "update_cache": {
            "latest_version": "0.1.0",
            "last_checked_at": "2024-01-01T00:00:00Z"
        }
    });
    std::fs::write(
        &index_path,
        serde_json::to_string_pretty(&v01_index).unwrap(),
    )
    .expect("write v0.1 profiles.json");

    // `list --json` must succeed and include the migrated profile.
    let out = env.run(&["list", "--json"]);
    let v: serde_json::Value = parse_json(&out);
    let profiles = v["profiles"].as_array().expect("profiles must be array");
    let ids: Vec<&str> = profiles.iter().filter_map(|p| p["id"].as_str()).collect();
    assert!(
        ids.contains(&id),
        "expected legacy profile id '{id}' in list output, got {ids:?}"
    );

    // The rewritten file must no longer contain legacy fields.
    let rewritten: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap())
            .expect("rewritten file must be valid JSON");
    assert!(
        rewritten.get("active_profile_id").is_none(),
        "rewritten index must not have 'active_profile_id'"
    );
    assert!(
        rewritten.get("update_cache").is_none(),
        "rewritten index must not have 'update_cache'"
    );
}

/// A v0.2.0-format profiles.json (`version: 1`, no `last_used`, no
/// `update_cache`) must load cleanly with profiles visible.
#[test]
fn compat_v02_profiles_index_loads_correctly() {
    let env = TestEnv::new();

    let id = "v02-id";
    write_profile_tokens(
        &env,
        id,
        serde_json::json!({
            "account_id": "acct-v02",
            "id_token": "tok-v02",
            "access_token": "tok-v02-access"
        }),
    );

    // Write a v0.2.0-style index: has version:1 and no newer metadata.
    let index_path = env.profiles_dir().join("profiles.json");
    let v02_index = serde_json::json!({
        "version": 1,
        "profiles": {
            id: {
                "email": "v02@example.com",
                "plan": "team",
                "label": "v02-work",
                "account_id": "acct-v02",
                "is_api_key": false
            }
        }
    });
    std::fs::write(
        &index_path,
        serde_json::to_string_pretty(&v02_index).unwrap(),
    )
    .expect("write v0.2 profiles.json");

    let out = env.run(&["list", "--json"]);
    let v: serde_json::Value = parse_json(&out);

    let profiles = v["profiles"].as_array().expect("profiles must be array");
    let ids: Vec<&str> = profiles.iter().filter_map(|p| p["id"].as_str()).collect();
    assert!(
        ids.contains(&id),
        "expected v0.2 profile id '{id}' in list output, got {ids:?}"
    );
}

/// Running `doctor --fix` against a v0.1.0-format index must:
/// * report normalisation in its repair log, and
/// * rewrite the file so it has `version: 3` and no legacy fields.
#[test]
fn compat_v01_doctor_fix_migrates_index() {
    let env = TestEnv::new();

    let id = "legacy-doctor-id";
    write_profile_tokens(
        &env,
        id,
        serde_json::json!({
            "account_id": "acct-doc",
            "id_token": "tok-doc",
            "access_token": "tok-doc-access"
        }),
    );

    let index_path = env.profiles_dir().join("profiles.json");
    let v01_index = serde_json::json!({
        "active_profile_id": id,
        "profiles": {
            id: {
                "last_used": 1_700_000_000u64,
                "added_at": 1u64
            }
        }
    });
    std::fs::write(
        &index_path,
        serde_json::to_string_pretty(&v01_index).unwrap(),
    )
    .expect("write v0.1 profiles.json for doctor test");

    // doctor --fix should succeed.
    let out = env.run(&["doctor", "--fix"]);
    let lower = out.to_lowercase();
    assert!(
        lower.contains("normaliz")
            || lower.contains("migrat")
            || lower.contains("repair")
            || lower.contains("fix"),
        "expected doctor --fix to report a normalisation/migration, got:\n{out}"
    );

    // Rewritten file must have version 3 and no legacy fields.
    let rewritten: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap())
            .expect("rewritten file must be valid JSON after doctor --fix");
    assert_eq!(
        rewritten["version"],
        serde_json::json!(3),
        "expected version:3 after doctor --fix, got {}",
        rewritten["version"]
    );
    assert!(
        rewritten.get("active_profile_id").is_none(),
        "doctor --fix must remove 'active_profile_id'"
    );
    assert!(
        rewritten.get("update_cache").is_none(),
        "doctor --fix must remove 'update_cache'"
    );
}
