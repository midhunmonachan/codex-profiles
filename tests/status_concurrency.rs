mod common;

use common::build_id_token;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

const A_ACCOUNT: &str = "acct-alpha";
const A_EMAIL: &str = "alpha@example.com";
const A_PLAN: &str = "team";
const A_OLD_ACCESS: &str = "alpha-old-access";
const A_OLD_REFRESH: &str = "alpha-old-refresh";
const A_NEW_ACCESS: &str = "alpha-refreshed-access";
const A_NEW_REFRESH: &str = "alpha-refreshed-refresh";
const B_ACCOUNT: &str = "acct-beta";
const B_EMAIL: &str = "beta@example.com";
const B_PLAN: &str = "team";
const B_ACCESS: &str = "beta-access";

struct TestEnv {
    home: tempfile::TempDir,
    bin_path: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::Builder::new()
            .prefix("codex-profiles-status-race-")
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

    fn profiles_dir(&self) -> PathBuf {
        self.codex_dir().join("profiles")
    }

    fn write_config(&self, base_url: &str) {
        fs::write(
            self.codex_dir().join("config.toml"),
            format!("chatgpt_base_url = \"{base_url}\"\n"),
        )
        .expect("write config");
    }

    fn write_auth_with_refresh(
        &self,
        account: &str,
        email: &str,
        plan: &str,
        access: &str,
        refresh: &str,
    ) {
        let value = serde_json::json!({
            "tokens": {
                "account_id": account,
                "id_token": build_id_token(email, plan),
                "access_token": access,
                "refresh_token": refresh,
            }
        });
        fs::write(
            self.codex_dir().join("auth.json"),
            serde_json::to_vec(&value).expect("serialize auth"),
        )
        .expect("write auth");
    }

    fn write_auth(&self, account: &str, email: &str, plan: &str, access: &str) {
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

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.bin_path);
        command
            .args(args)
            .env_remove("CODEX_HOME")
            .env_remove("CODEX_REFRESH_TOKEN_URL_OVERRIDE")
            .env("HOME", self.home_path())
            .env("CODEX_PROFILES_HOME", self.home_path())
            .env("CODEX_PROFILES_COMMAND", "codex-profiles")
            .env("CODEX_PROFILES_SKIP_UPDATE", "1")
            .env("NO_COLOR", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> String {
        let output = self.command(args).output().expect("run command");
        assert_success(args, &output);
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn spawn_status(&self, refresh_url: &str) -> Child {
        let mut command = self.command(&["status"]);
        command.env("CODEX_REFRESH_TOKEN_URL_OVERRIDE", refresh_url);
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn status")
    }

    fn read_auth(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.codex_dir().join("auth.json")).expect("read auth"))
            .expect("parse auth")
    }

    fn read_profile(&self, id: &str) -> serde_json::Value {
        serde_json::from_slice(
            &fs::read(self.profiles_dir().join(format!("{id}.json"))).expect("read profile"),
        )
        .expect("parse profile")
    }
}

fn assert_success(args: &[&str], output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn resolve_bin_path() -> PathBuf {
    if let Ok(path) = env::var("CARGO_BIN_EXE_codex-profiles") {
        let path = PathBuf::from(path);
        if path.exists() {
            return path;
        }
    }
    let exe = env::current_exe().expect("current exe");
    exe.parent()
        .and_then(Path::parent)
        .expect("target dir")
        .join(if cfg!(windows) {
            "codex-profiles.exe"
        } else {
            "codex-profiles"
        })
}

fn read_request(stream: &mut TcpStream) {
    let mut data = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let count = stream.read(&mut chunk).expect("read request");
        if count == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..count]);
        if data.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
}

fn accept_with_deadline(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .expect("set read timeout");
                return stream;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "timed out waiting for request");
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("accept request: {err}"),
        }
    }
}

fn usage_body() -> &'static str {
    r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#
}

fn response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn start_usage_race_server() -> (SocketAddr, Receiver<()>, Sender<()>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind usage server");
    listener
        .set_nonblocking(true)
        .expect("set usage listener nonblocking");
    let addr = listener.local_addr().expect("usage server address");
    let (seen_tx, seen_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut first = accept_with_deadline(&listener);
        read_request(&mut first);
        first
            .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
            .expect("write usage 401");

        let mut second = accept_with_deadline(&listener);
        read_request(&mut second);
        seen_tx.send(()).expect("signal second usage request");
        release_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("release second usage request");
        second
            .write_all(response("200 OK", usage_body()).as_bytes())
            .expect("write usage 200");
    });
    (addr, seen_rx, release_tx, handle)
}

fn start_refresh_server() -> (SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind refresh server");
    listener
        .set_nonblocking(true)
        .expect("set refresh listener nonblocking");
    let addr = listener.local_addr().expect("refresh server address");
    let body = serde_json::json!({
        "id_token": build_id_token(A_EMAIL, A_PLAN),
        "access_token": A_NEW_ACCESS,
        "refresh_token": A_NEW_REFRESH,
    })
    .to_string();
    let handle = thread::spawn(move || {
        let mut stream = accept_with_deadline(&listener);
        read_request(&mut stream);
        stream
            .write_all(response("200 OK", &body).as_bytes())
            .expect("write refresh response");
    });
    (addr, handle)
}

#[test]
fn status_refresh_does_not_copy_loaded_account_into_old_profile() {
    let env = TestEnv::new();
    env.write_config("http://127.0.0.1:1/backend-api");

    env.write_auth_with_refresh(A_ACCOUNT, A_EMAIL, A_PLAN, A_OLD_ACCESS, A_OLD_REFRESH);
    env.run(&["save", "--label", "alpha"]);
    env.write_auth(B_ACCOUNT, B_EMAIL, B_PLAN, B_ACCESS);
    env.run(&["save", "--label", "beta"]);
    env.write_auth_with_refresh(A_ACCOUNT, A_EMAIL, A_PLAN, A_OLD_ACCESS, A_OLD_REFRESH);

    let (usage_addr, usage_seen, release_usage, usage_handle) = start_usage_race_server();
    let (refresh_addr, refresh_handle) = start_refresh_server();
    env.write_config(&format!("http://{usage_addr}/backend-api"));

    let refresh_url = format!("http://{refresh_addr}/token");
    let status = env.spawn_status(&refresh_url);
    usage_seen
        .recv_timeout(Duration::from_secs(10))
        .expect("status reached post-refresh usage request");

    let load_output = env.run(&["load", "--label", "beta"]);
    assert!(load_output.contains("Loaded"), "{load_output}");

    release_usage.send(()).expect("release usage response");
    let status_output = status.wait_with_output().expect("wait for status");
    assert_success(&["status"], &status_output);

    usage_handle.join().expect("join usage server");
    refresh_handle.join().expect("join refresh server");

    let auth = env.read_auth();
    assert_eq!(auth["tokens"]["account_id"], B_ACCOUNT);
    assert_eq!(auth["tokens"]["access_token"], B_ACCESS);

    let alpha = env.read_profile("alpha@example.com-team");
    assert_eq!(alpha["tokens"]["account_id"], A_ACCOUNT);
    assert_eq!(alpha["tokens"]["access_token"], A_NEW_ACCESS);
    assert_eq!(alpha["tokens"]["refresh_token"], A_NEW_REFRESH);

    let beta = env.read_profile("beta@example.com-team");
    assert_eq!(beta["tokens"]["account_id"], B_ACCOUNT);
    assert_eq!(beta["tokens"]["access_token"], B_ACCESS);
}
