mod common;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

fn status_response(body: &str, status: &str, headers: &str) -> (serde_json::Value, Duration) {
    let (output, elapsed) = run_status(body, status, headers, &["status", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (serde_json::from_slice(&output.stdout).unwrap(), elapsed)
}

fn text_status_response(body: &str, status: &str, headers: &str) -> String {
    let (output, _) = run_status(body, status, headers, &["status"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn run_status(body: &str, status: &str, headers: &str, args: &[&str]) -> (Output, Duration) {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    );
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&buffer[..read]);
        }
        stream.write_all(response.as_bytes()).unwrap();
    });
    fs::write(
        dir.path().join("config.toml"),
        format!("chatgpt_base_url = 'http://{addr}/backend-api'\n"),
    )
    .unwrap();
    fs::write(
        dir.path().join("auth.json"),
        serde_json::json!({
            "tokens": {
                "id_token": common::build_id_token("usage@example.com", "plus"),
                "account_id": "usage-test-account",
                "access_token": "synthetic-usage-token"
            }
        })
        .to_string(),
    )
    .unwrap();
    let start = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_codex-profiles"))
        .args(args)
        .env("CODEX_HOME", dir.path())
        .env("CODEX_PROFILES_SKIP_UPDATE", "1")
        .env("NO_COLOR", "1")
        .env_remove("CODEX_PROFILES_HOME")
        .output()
        .unwrap();
    let elapsed = start.elapsed();
    server.join().unwrap();
    (output, elapsed)
}

#[test]
fn status_json_reports_weekly_only_limits_without_a_five_hour_alias() {
    let (status, _) = status_response(
        r#"{"rate_limit":{"secondary_window":{"used_percent":40,"limit_window_seconds":604800,"reset_at":2000000000}}}"#,
        "200 OK",
        "",
    );
    let bucket = &status["usage"]["buckets"][0];
    assert_eq!(status["usage"]["state"], "ok");
    assert!(bucket["primary"].is_null());
    assert!(bucket["five_hour"].is_null());
    assert_eq!(bucket["secondary"]["window_seconds"], 604800);
    assert_eq!(bucket["weekly"]["left_percent"], 60);
}

#[test]
fn status_json_preserves_custom_duration() {
    let (status, _) = status_response(
        r#"{"rate_limit":{"primary_window":{"used_percent":10,"limit_window_seconds":900,"reset_at":2000000000}}}"#,
        "200 OK",
        "",
    );
    let bucket = &status["usage"]["buckets"][0];
    assert_eq!(bucket["primary"]["window_seconds"], 900);
    assert_eq!(bucket["primary"]["left_percent"], 90);
    assert!(bucket["five_hour"].is_null());
    assert!(bucket["weekly"].is_null());
}

#[test]
fn status_json_marks_missing_windows_unavailable() {
    for body in [
        "{}",
        r#"{"rate_limit":{}}"#,
        r#"{"additional_rate_limits":[{"metered_feature":"empty","rate_limit":null}]}"#,
    ] {
        let (status, _) = status_response(body, "200 OK", "");
        assert_eq!(status["usage"]["state"], "unavailable");
        assert!(status["usage"].get("buckets").is_none());
        assert_eq!(status["usage"]["summary"], "Data not available");
    }
}

#[test]
fn status_text_formats_upstream_windows_in_the_normal_view() {
    let output = text_status_response(
        r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000},"secondary_window":{"used_percent":50,"limit_window_seconds":604800,"reset_at":2000600000}}}"#,
        "200 OK",
        "",
    );
    assert!(output.contains("5 hour:"), "{output}");
    assert!(output.contains("Weekly:"), "{output}");
    assert!(output.contains("80% left"), "{output}");
}

#[test]
fn status_returns_long_retry_after_response_without_waiting_or_retrying_early() {
    let (status, elapsed) = status_response(
        r#"{"error":{"message":"Try later"}}"#,
        "429 Too Many Requests",
        "Retry-After: 86400\r\n",
    );
    assert!(elapsed < Duration::from_secs(5), "status took {elapsed:?}");
    assert_eq!(status["usage"]["state"], "error");
    assert_eq!(status["error"]["status_code"], 429, "{status}");
}
