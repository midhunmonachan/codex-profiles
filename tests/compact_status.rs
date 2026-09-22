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
const BETA_ACCOUNT: &str = "acct-beta";
const BETA_EMAIL: &str = "beta@example.com";
const BETA_PLAN: &str = "team";
const BETA_TOKEN: &str = "token-beta";

static COMMAND_MUTEX: Mutex<()> = Mutex::new(());

struct TestEnv {
    home: tempfile::TempDir,
    bin_path: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::Builder::new()
            .prefix("codex-profiles-compact-test-")
            .tempdir()
            .expect("create temp home");
        fs::create_dir_all(home.path().join(".codex")).expect("create codex dir");

        let source_bin = resolve_bin_path();
        let bin_dir = home.path().join(".test-bin");
        fs::create_dir_all(&bin_dir).expect("create test bin dir");
        let bin_name = source_bin.file_name().expect("binary file name");
        let bin_path = bin_dir.join(bin_name);
        fs::copy(&source_bin, &bin_path).expect("copy test binary");

        Self { home, bin_path }
    }

    fn home_path(&self) -> &Path {
        self.home.path()
    }

    fn codex_dir(&self) -> PathBuf {
        self.home_path().join(".codex")
    }

    fn write_config(&self, base_url: &str) {
        fs::write(
            self.codex_dir().join("config.toml"),
            format!("chatgpt_base_url = \"{base_url}\"\n"),
        )
        .expect("write config");
    }

    fn write_oauth_auth(&self, account: &str, email: &str, plan: &str, access: &str) {
        let value = serde_json::json!({
            "tokens": {
                "account_id": account,
                "id_token": build_id_token(email, plan),
                "access_token": access,
            }
        });
        fs::write(
            self.codex_dir().join("auth.json"),
            serde_json::to_vec(&value).expect("serialize auth"),
        )
        .expect("write auth");
    }

    fn write_api_key_auth(&self, api_key: &str) {
        let value = serde_json::json!({ "OPENAI_API_KEY": api_key });
        fs::write(
            self.codex_dir().join("auth.json"),
            serde_json::to_vec(&value).expect("serialize api key auth"),
        )
        .expect("write api key auth");
    }

    fn run_output(&self, args: &[&str]) -> Output {
        let _guard = COMMAND_MUTEX.lock().expect("command mutex");
        Command::new(&self.bin_path)
            .args(args)
            .env_remove("CODEX_HOME")
            .env("HOME", self.home_path())
            .env("CODEX_PROFILES_HOME", self.home_path())
            .env("CODEX_PROFILES_COMMAND", "codex-profiles")
            .env("CODEX_PROFILES_SKIP_UPDATE", "1")
            .env("NO_COLOR", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .expect("run command")
    }

    fn run(&self, args: &[&str]) -> String {
        let output = self.run_output(args);
        assert!(
            output.status.success(),
            "command failed: {args:?}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        ascii_only(String::from_utf8_lossy(&output.stdout).as_ref())
    }

    fn run_expect_error(&self, args: &[&str]) -> String {
        let output = self.run_output(args);
        assert!(!output.status.success(), "command unexpectedly succeeded");
        ascii_only(String::from_utf8_lossy(&output.stderr).as_ref())
    }
}

fn resolve_bin_path() -> PathBuf {
    if let Ok(path) = env::var("CARGO_BIN_EXE_codex-profiles") {
        let path = PathBuf::from(path);
        if path.exists() {
            return path;
        }
    }
    let exe = env::current_exe().expect("current exe");
    let target_dir = exe.parent().and_then(Path::parent).expect("target dir");
    target_dir.join(if cfg!(windows) {
        "codex-profiles.exe"
    } else {
        "codex-profiles"
    })
}

fn ascii_only(raw: &str) -> String {
    raw.replace('\r', "")
        .chars()
        .filter(|ch| ch.is_ascii())
        .collect::<String>()
}

fn start_responses(
    responses: Vec<String>,
    max_requests: usize,
) -> std::io::Result<(SocketAddr, thread::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let responses = responses
        .into_iter()
        .map(String::into_bytes)
        .collect::<Vec<_>>();
    let handle = thread::spawn(move || {
        let mut handled = 0usize;
        let mut last_activity = Instant::now();
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut request = [0u8; 2048];
                    let _ = stream.read(&mut request);
                    if responses.is_empty() {
                        break;
                    }
                    let response = &responses[handled.min(responses.len() - 1)];
                    let _ = stream.write_all(response);
                    handled += 1;
                    last_activity = Instant::now();
                    if handled >= max_requests {
                        break;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    let timeout = if handled == 0 {
                        Duration::from_secs(30)
                    } else {
                        Duration::from_secs(5)
                    };
                    if last_activity.elapsed() > timeout {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    Ok((addr, handle))
}

fn ok_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

fn error_response() -> String {
    let body = "server unavailable";
    format!(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

fn save_profile(env: &TestEnv, account: &str, email: &str, plan: &str, token: &str, label: &str) {
    env.write_oauth_auth(account, email, plan, token);
    env.run(&["save", "--label", label]);
}

#[test]
fn compact_requires_all_and_conflicts_with_json() {
    let env = TestEnv::new();
    let err = env.run_expect_error(&["status", "--compact"]);
    assert!(err.contains("--compact") && err.contains("--all"), "{err}");

    let err = env.run_expect_error(&["status", "--all", "--compact", "--json"]);
    assert!(err.contains("--compact") && err.contains("--json"), "{err}");

    let err = env.run_expect_error(&["status", "--all", "--compact", "--label", "alpha"]);
    assert!(err.contains("--all") && err.contains("--label"), "{err}");
}

#[test]
fn compact_status_groups_buckets_and_uses_upstream_window_durations() {
    let env = TestEnv::new();
    save_profile(
        &env,
        ALPHA_ACCOUNT,
        ALPHA_EMAIL,
        ALPHA_PLAN,
        ALPHA_TOKEN,
        "alpha",
    );
    save_profile(
        &env,
        BETA_ACCOUNT,
        BETA_EMAIL,
        BETA_PLAN,
        BETA_TOKEN,
        "beta",
    );

    let body = r#"{
        "rate_limit": {
            "primary_window": {"used_percent": 20, "limit_window_seconds": 18000, "reset_at": 2000000000},
            "secondary_window": {"used_percent": 50, "limit_window_seconds": 604800, "reset_at": 2000600000}
        },
        "additional_rate_limits": [{
            "metered_feature": "review",
            "limit_name": "code-review",
            "rate_limit": {
                "primary_window": {"used_percent": 40, "limit_window_seconds": 3600, "reset_at": 2000001200}
            }
        }]
    }"#;
    let response = ok_response(body);
    let (addr, handle) = start_responses(vec![response], 2).expect("usage server");
    env.write_config(&format!("http://{addr}/backend-api"));

    let output = env.run(&["status", "--all", "--compact", "--plain"]);
    assert!(output.contains("alpha@example.com"), "{output}");
    assert!(output.contains("beta@example.com"), "{output}");
    assert!(output.contains("<- active"), "{output}");
    assert!(output.contains("codex: 5h 80% left (resets"), "{output}");
    assert!(output.contains("7d 50% left (resets"), "{output}");
    assert!(
        output.contains("code-review: 1h 60% left (resets"),
        "{output}"
    );
    assert!(!output.contains("5 hour:"), "{output}");
    handle.join().expect("join usage server");
}

#[test]
fn status_redacts_invalid_base_url_query_in_compact_and_normal_views() {
    let env = TestEnv::new();
    save_profile(
        &env,
        ALPHA_ACCOUNT,
        ALPHA_EMAIL,
        ALPHA_PLAN,
        ALPHA_TOKEN,
        "alpha",
    );
    let secret_url = "https://example.com/backend-api?api_key=compact-secret";
    env.write_config(secret_url);

    let compact = env.run(&["status", "--all", "--compact", "--plain"]);
    assert!(
        compact.contains("Unsupported chatgpt_base_url"),
        "{compact}"
    );
    assert!(!compact.contains("compact-secret"), "{compact}");
    assert!(!compact.contains(secret_url), "{compact}");

    let normal = env.run(&["status", "--all", "--plain"]);
    assert!(normal.contains("Unsupported chatgpt_base_url"), "{normal}");
    assert!(!normal.contains("compact-secret"), "{normal}");
    assert!(!normal.contains(secret_url), "{normal}");
}

#[test]
fn compact_status_keeps_api_key_and_usage_errors_visible() {
    let env = TestEnv::new();
    save_profile(
        &env,
        ALPHA_ACCOUNT,
        ALPHA_EMAIL,
        ALPHA_PLAN,
        ALPHA_TOKEN,
        "alpha",
    );
    env.write_api_key_auth("sk-proj-compact-test-key");
    env.run(&["save", "--label", "api"]);

    let response = error_response();
    let (addr, handle) = start_responses(vec![response], 3).expect("usage server");
    env.write_config(&format!("http://{addr}/backend-api"));

    let output = env.run(&["status", "--all", "--compact"]);
    assert!(output.contains("Usage unavailable for API key"), "{output}");
    assert!(
        output.contains("Rate-limit usage data is only available"),
        "{output}"
    );
    assert!(output.contains("Usage error:"), "{output}");
    assert!(output.contains("alpha@example.com"), "{output}");
    assert!(output.contains("api"), "{output}");
    handle.join().expect("join usage server");
}
