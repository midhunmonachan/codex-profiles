use super::*;
use crate::auth::Tokens;
use crate::test_utils::{
    ENV_MUTEX, build_id_token, http_ok_response, make_paths, set_env_guard, spawn_server,
};
use crate::usage::{UsageFetchError, UsageSnapshotBucket, UsageSnapshotWindow};
use chrono::{Local, TimeZone};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;

fn ctx(root: &Path, show_usage: bool, show_id: bool, use_color: bool) -> ListCtx {
    ListCtx {
        base_url: None,
        base_url_error: None,
        now: Local.timestamp_opt(1_750_000_000, 0).single().unwrap(),
        show_usage,
        show_current_marker: true,
        show_id,
        use_color,
        profiles_dir: root.join("profiles"),
    }
}

fn token_set(email: &str, plan: &str, account_id: Option<&str>, access: Option<&str>) -> Tokens {
    Tokens {
        account_id: account_id.map(str::to_string),
        id_token: Some(build_id_token(email, plan)),
        access_token: access.map(str::to_string),
        refresh_token: Some("refresh-token".to_string()),
    }
}

fn api_key_tokens() -> Tokens {
    Tokens {
        account_id: Some("api-key-test".to_string()),
        id_token: None,
        access_token: None,
        refresh_token: None,
    }
}

fn window(seconds: i64, left_percent: i64) -> UsageSnapshotWindow {
    UsageSnapshotWindow {
        left_percent,
        reset_at: 1_750_000_600,
        window_seconds: seconds,
    }
}

fn bucket(
    label: &str,
    primary: Option<UsageSnapshotWindow>,
    secondary: Option<UsageSnapshotWindow>,
) -> UsageSnapshotBucket {
    UsageSnapshotBucket {
        id: label.to_ascii_lowercase(),
        label: label.to_string(),
        five_hour: primary.clone().filter(|item| item.window_seconds == 18_000),
        weekly: secondary
            .clone()
            .filter(|item| item.window_seconds == 604_800),
        primary,
        secondary,
    }
}

fn entry(display: &str) -> Entry {
    Entry {
        id: Some("profile-id".to_string()),
        label: Some("work".to_string()),
        email: Some("user@example.com".to_string()),
        plan: Some("Pro".to_string()),
        is_api_key: false,
        is_saved: true,
        display: display.to_string(),
        details: Vec::new(),
        warnings: Vec::new(),
        usage: None,
        error_summary: None,
        always_show_details: false,
        is_current: false,
    }
}

fn spawn_usage_sequence(responses: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    format!("http://{address}")
}

#[test]
fn renderers_cover_headers_details_compact_and_empty_paths() {
    let dir = tempfile::tempdir().unwrap();
    let mut error = entry("broken");
    error.error_summary = Some("Error summary".to_string());
    error.is_current = true;
    let mut detailed = entry("detailed");
    detailed.id = Some("detail-id".to_string());
    detailed.details = vec!["first\n\nthird".to_string(), String::new()];
    detailed.always_show_details = true;

    let plain_ctx = ctx(dir.path(), false, true, false);
    let lines = render_entries(&[error.clone(), detailed.clone()], &plain_ctx, true);
    assert!(lines.iter().any(|line| line.contains("[id: profile-id]")));
    assert!(lines.iter().any(|line| line.contains("Error summary")));
    assert!(lines.iter().any(|line| line == " first"));
    assert!(lines.iter().any(|line| line == "third"));
    assert!(lines.iter().filter(|line| line.is_empty()).count() >= 2);

    let compact = render_compact_entries(&[error], &plain_ctx);
    assert!(compact.iter().any(|line| line.contains("Error summary")));

    let mut no_usage = entry("no usage");
    no_usage.id = None;
    no_usage.error_summary = None;
    let compact = render_compact_entries(&[no_usage], &plain_ctx);
    assert!(
        compact
            .iter()
            .any(|line| line.contains("Usage unavailable"))
    );

    assert_eq!(render_entry_details(&detailed)[0], " first");
    assert_eq!(format_window_duration(86_400), "1d");
    assert_eq!(format_window_duration(3_600), "1h");
    assert_eq!(format_window_duration(60), "1m");
    assert_eq!(format_window_duration(7), "7s");
    assert_eq!(format_window_duration(0), "unknown window");
}

#[test]
fn colored_renderers_include_ids_and_current_markers() {
    colored::control::set_override(true);
    let mut current = entry("current@example.com");
    current.is_current = true;
    let context = ctx(Path::new("/tmp"), true, true, true);
    let lines = render_entries(std::slice::from_ref(&current), &context, true);
    let compact = render_compact_entries(&[current], &context);
    colored::control::unset_override();

    assert!(crate::ui::strip_ansi(&lines[0]).contains("[id: profile-id]"));
    assert!(crate::ui::strip_ansi(&lines[0]).contains("<- active"));
    assert!(crate::ui::strip_ansi(&compact[0]).contains("<- active"));
}

#[test]
fn compact_renderer_formats_buckets_warnings_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut ok = entry("account");
    ok.is_current = true;
    ok.warnings = vec!["Warning: unsaved\nsecond".to_string()];
    ok.usage = Some(StatusUsageJson::ok(vec![
        bucket(
            "Primary",
            Some(window(18_000, 80)),
            Some(window(604_800, 65)),
        ),
        bucket("Empty", None, None),
    ]));

    let mut failed = entry("failed");
    failed.usage = Some(StatusUsageJson::from_message(
        "error",
        Some(429),
        "Usage failed\ntry again later",
    ));
    failed.error_summary = Some("Usage error: Usage failed".to_string());

    let lines = render_compact_entries(&[ok, failed], &ctx(dir.path(), true, false, false));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Primary: 5h 80% left"))
    );
    assert!(lines.iter().any(|line| line.contains("7d 65% left")));
    assert!(lines.iter().any(|line| line.contains("Empty: unavailable")));
    assert!(lines.iter().any(|line| line.contains("Warning: unsaved")));
    assert!(lines.iter().any(|line| line.contains("Usage error")));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("    try again later"))
    );

    let mut empty_ok = entry("empty ok");
    empty_ok.usage = Some(StatusUsageJson {
        state: "ok",
        buckets: Vec::new(),
        status_code: None,
        summary: None,
        detail: None,
    });
    let empty_lines = render_compact_entries(&[empty_ok], &ctx(dir.path(), true, false, false));
    assert!(
        empty_lines
            .iter()
            .any(|line| line.contains("Usage unavailable"))
    );

    let unknown = bucket("Unknown", Some(window(-1, 0)), None);
    assert!(format_compact_bucket(&unknown, Local::now()).contains("unknown window"));
}

#[test]
fn status_messages_and_json_extractors_cover_multiline_and_embedded_objects() {
    let unavailable = unavailable_lines("Usage unavailable\nreason one\n\nreason two", false);
    assert!(unavailable[0].contains("Usage unavailable"));
    assert!(unavailable.iter().any(|line| line.contains("reason one")));
    assert!(unavailable.iter().any(|line| line.contains("reason two")));

    assert!(plain_error_lines("", false).is_empty());
    let merged = plain_error_lines("Request failed\nunexpected status 401\nbody", false);
    assert!(merged[0].contains("unexpected status 401"));
    let continued = plain_error_lines("Request failed\nbody", true);
    assert!(continued[1].contains("body"));

    let (summary, detail) = usage_message_parts("Error: headline\n\n detail");
    assert_eq!(summary, "headline");
    assert_eq!(detail.as_deref(), Some("detail"));
    assert_eq!(usage_message_parts("").0, "");

    let parsed = status_error_summary_json("Request failed: {\"status\": 401}, retry".into());
    assert_eq!(parsed.message, "Request failed, retry");
    assert_eq!(parsed.response.unwrap()["status"], 401);
    assert_eq!(status_error_summary_json("plain".into()).message, "plain");
    assert!(extract_embedded_json_object("prefix {\"text\":\"} escaped\"}").is_some());
    assert!(extract_embedded_json_object("{invalid {\"ok\":true}").is_some());
    assert!(extract_embedded_json_object("{\"text\":\"escaped \\\" quote\"}").is_some());
    assert!(extract_embedded_json_object("prefix {not-json}").is_none());
    assert!(find_json_object_end("}", 0).is_none());
    assert!(find_json_object_end("{", 0).is_none());
    assert_eq!(strip_embedded_json_segment("{\"a\":1}", 0, 7), "");
    assert_eq!(
        strip_embedded_json_segment("{\"a\":1}, right", 0, 7),
        "right"
    );
    assert_eq!(
        strip_embedded_json_segment("left: {\"a\":1}", 6, 13),
        "left"
    );
    assert_eq!(
        strip_embedded_json_segment("left: {\"a\":1}, right", 6, 13),
        "left, right"
    );
}

#[test]
fn status_usage_json_and_profile_json_preserve_structured_errors() {
    let empty = StatusUsageJson::ok(Vec::new());
    assert_eq!(empty.state, "unavailable");
    let from_message = StatusUsageJson::from_message("error", Some(500), "Headline\nDetail");
    assert_eq!(from_message.detail.as_deref(), Some("Detail"));
    let parse = UsageFetchError::Parse("malformed".to_string());
    let fetch = StatusUsageJson::from_fetch_error(&parse);
    assert_eq!(fetch.state, "error");
    assert!(fetch.summary.unwrap().contains("malformed"));
    assert_eq!(StatusUsageJson::unavailable("offline").state, "unavailable");

    let mut usage_error = entry("error");
    usage_error.warnings = vec!["warn\u{1b}[31m".to_string()];
    usage_error.usage = Some(from_message);
    usage_error.error_summary = Some("Top level {\"code\": 1}".to_string());
    let json = status_profile_json(usage_error);
    let error = json.error.unwrap();
    assert_eq!(error.status_code, Some(500));
    assert_eq!(error.detail.as_deref(), Some("Detail"));
    assert_eq!(error.summary.message, "Top level");
    assert_eq!(error.summary.response.unwrap()["code"], 1);

    let mut ok_with_summary = entry("ok");
    ok_with_summary.usage = Some(StatusUsageJson::ok(vec![bucket(
        "primary",
        Some(window(18_000, 100)),
        None,
    )]));
    ok_with_summary.error_summary = Some("Top error".to_string());
    assert_eq!(
        status_profile_json(ok_with_summary)
            .error
            .unwrap()
            .summary
            .message,
        "Top error"
    );
}

#[test]
fn detail_lines_handles_api_keys_missing_credentials_and_bad_config() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("profile.json");

    let api = api_key_tokens();
    let no_usage = detail_lines(
        &mut api.clone(),
        None,
        None,
        &ctx(dir.path(), false, false, false),
        &source,
    );
    assert!(no_usage.0.is_empty());
    let api_usage = detail_lines(
        &mut api.clone(),
        None,
        None,
        &ctx(dir.path(), true, false, false),
        &source,
    );
    assert_eq!(api_usage.2.as_ref().unwrap().state, "unavailable");

    let missing_access = token_set("user@example.com", "Pro", Some("account"), None);
    let unavailable = detail_lines(
        &mut missing_access.clone(),
        Some("user@example.com"),
        Some("Pro"),
        &ctx(dir.path(), true, false, false),
        &source,
    );
    assert_eq!(unavailable.2.as_ref().unwrap().state, "unavailable");

    let missing_identity = token_set("user@example.com", "Pro", Some("account"), Some("access"));
    let mut identity_ctx = ctx(dir.path(), true, false, false);
    identity_ctx.base_url = None;
    let identity_only = detail_lines(
        &mut missing_identity.clone(),
        None,
        None,
        &identity_ctx,
        &source,
    );
    assert!(identity_only.0.is_empty());
    assert!(identity_only.2.is_none());

    let mut invalid_ctx = ctx(dir.path(), true, false, false);
    invalid_ctx.base_url_error = Some("bad base URL".to_string());
    let mut valid = token_set("user@example.com", "Pro", Some("account"), Some("access"));
    let bad_config = detail_lines(
        &mut valid.clone(),
        Some("user@example.com"),
        Some("Pro"),
        &invalid_ctx,
        &source,
    );
    assert!(
        bad_config
            .0
            .iter()
            .any(|line| line.contains("bad base URL"))
    );
    assert_eq!(bad_config.2.as_ref().unwrap().state, "error");

    let mut no_base_url = ctx(dir.path(), true, false, false);
    no_base_url.base_url = None;
    no_base_url.base_url_error = None;
    let no_url = detail_lines(
        &mut valid,
        Some("user@example.com"),
        Some("Pro"),
        &no_base_url,
        &source,
    );
    assert!(no_url.0.is_empty());
    assert!(no_url.2.is_none());

    let payload = r#"{"rate_limit":{"primary_window":{"used_percent":20.0,"limit_window_seconds":3600,"reset_at":1750000600}}}"#;
    let usage_url = spawn_usage_sequence(vec![http_ok_response(payload, "application/json")]);
    let mut reachable = ctx(dir.path(), true, false, false);
    reachable.base_url = Some(format!("{usage_url}/backend-api"));
    let mut account = token_set("user@example.com", "Pro", Some("account"), Some("access"));
    let fetched = detail_lines(
        &mut account,
        Some("user@example.com"),
        Some("Pro"),
        &reachable,
        &source,
    );
    assert_eq!(fetched.2.as_ref().unwrap().state, "ok");
}

#[test]
fn make_entry_and_current_handle_missing_files_errors_and_unsaved_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let profile_path = dir.path().join("profiles").join("missing.json");
    let mut plain_ctx = ctx(dir.path(), false, false, false);
    let index_entry = ProfileIndexEntry {
        email: Some("cached@example.com".to_string()),
        plan: Some("Pro".to_string()),
        ..ProfileIndexEntry::default()
    };
    let bad = Err("broken profile".to_string());
    let bad_entry = make_entry(
        Some("broken".to_string()),
        Some(&bad),
        Some(&index_entry),
        &profile_path,
        &plain_ctx,
        false,
    );
    assert!(bad_entry.error_summary.unwrap().contains("broken profile"));
    let missing = make_entry(
        None,
        None,
        Some(&index_entry),
        &profile_path,
        &plain_ctx,
        false,
    );
    assert!(missing.error_summary.unwrap().contains("missing"));

    let paths = make_paths(dir.path());
    assert!(make_current(&paths, None, &Labels::new(), &BTreeMap::new(), &plain_ctx,).is_none());

    fs::create_dir_all(&paths.codex).unwrap();
    fs::write(&paths.auth, "not-json").unwrap();
    let read_error =
        make_current(&paths, None, &Labels::new(), &BTreeMap::new(), &plain_ctx).unwrap();
    assert!(read_error.error_summary.is_some());

    let current = token_set(
        "current@example.com",
        "Pro",
        Some("current"),
        Some("access"),
    );
    fs::write(
        &paths.auth,
        serde_json::json!({
            "tokens": {
                "account_id": current.account_id,
                "id_token": current.id_token,
                "access_token": current.access_token,
                "refresh_token": current.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    let mut map = BTreeMap::new();
    map.insert(
        "other".to_string(),
        Ok(token_set(
            "other@example.com",
            "Pro",
            Some("other"),
            Some("a"),
        )),
    );
    let unmatched = make_current(&paths, Some("other"), &Labels::new(), &map, &plain_ctx).unwrap();
    assert!(unmatched.id.is_none());
    assert!(!unmatched.warnings.is_empty());

    let mut hint_map = BTreeMap::new();
    hint_map.insert(
        "no-identity".to_string(),
        Ok(Tokens {
            account_id: None,
            id_token: None,
            access_token: None,
            refresh_token: None,
        }),
    );
    let absent_hint = make_current(
        &paths,
        Some("missing-hint"),
        &Labels::new(),
        &hint_map,
        &plain_ctx,
    )
    .unwrap();
    assert!(absent_hint.id.is_none());
    let candidate_without_identity = make_current(
        &paths,
        Some("no-identity"),
        &Labels::new(),
        &hint_map,
        &plain_ctx,
    )
    .unwrap();
    assert!(candidate_without_identity.id.is_none());

    plain_ctx.use_color = true;
    let colored = make_current(&paths, None, &Labels::new(), &BTreeMap::new(), &plain_ctx).unwrap();
    assert!(colored.details.len() >= 2);
}

#[test]
fn make_entries_uses_single_usage_worker_without_network() {
    let _env_lock = ENV_MUTEX.lock().unwrap();
    let _concurrency = set_env_guard(USAGE_CONCURRENCY_ENV, Some("1"));
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = Snapshot {
        labels: Labels::new(),
        tokens: BTreeMap::new(),
        index: ProfilesIndex::default(),
    };
    for id in ["one", "two", "three"] {
        snapshot.tokens.insert(
            id.to_string(),
            Ok(token_set(
                "user@example.com",
                "Pro",
                Some(id),
                Some("access"),
            )),
        );
    }
    let entries = make_entries(
        &["one".to_string(), "two".to_string(), "three".to_string()],
        &snapshot,
        None,
        &ctx(dir.path(), true, false, false),
    );
    assert_eq!(entries.len(), 3);
}

#[test]
fn make_entries_falls_back_when_usage_pool_cannot_build() {
    let _env_lock = ENV_MUTEX.lock().unwrap();
    let _concurrency = set_env_guard(USAGE_CONCURRENCY_ENV, Some("2"));
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = Snapshot {
        labels: Labels::new(),
        tokens: BTreeMap::new(),
        index: ProfilesIndex::default(),
    };
    for id in ["one", "two", "three"] {
        snapshot.tokens.insert(
            id.to_string(),
            Ok(token_set(
                "user@example.com",
                "Pro",
                Some(id),
                Some("access"),
            )),
        );
    }
    FORCE_USAGE_POOL_FALLBACK.with(|fallback| fallback.set(true));
    let entries = make_entries(
        &["one".to_string(), "two".to_string(), "three".to_string()],
        &snapshot,
        None,
        &ctx(dir.path(), true, false, false),
    );
    FORCE_USAGE_POOL_FALLBACK.with(|fallback| fallback.set(false));
    assert_eq!(entries.len(), 3);
}

#[test]
fn make_entries_uses_parallel_usage_pool() {
    let _env_lock = ENV_MUTEX.lock().unwrap();
    let _concurrency = set_env_guard(USAGE_CONCURRENCY_ENV, Some("2"));
    let dir = tempfile::tempdir().unwrap();
    let mut snapshot = Snapshot {
        labels: Labels::new(),
        tokens: BTreeMap::new(),
        index: ProfilesIndex::default(),
    };
    for id in ["one", "two", "three"] {
        snapshot.tokens.insert(
            id.to_string(),
            Ok(token_set(
                "user@example.com",
                "Pro",
                Some(id),
                Some("access"),
            )),
        );
    }
    let entries = make_entries(
        &["one".to_string(), "two".to_string(), "three".to_string()],
        &snapshot,
        None,
        &ctx(dir.path(), true, false, false),
    );
    assert_eq!(entries.len(), 3);
}

#[test]
fn empty_list_and_status_dispatches_are_stable() {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).unwrap();

    list_profiles(&paths, false, false).unwrap();
    list_profiles(&paths, true, false).unwrap();
    status_profiles(&paths, false, false, None, None, false).unwrap();
    status_profiles(&paths, false, false, None, None, true).unwrap();
    status_profiles(&paths, true, false, None, None, false).unwrap();
    status_profiles(&paths, true, true, None, None, false).unwrap();
    status_profiles(&paths, true, false, None, None, true).unwrap();
    status_profiles(
        &paths,
        false,
        false,
        Some("missing".to_string()),
        None,
        false,
    )
    .unwrap();
    status_profiles(
        &paths,
        false,
        false,
        Some("missing".to_string()),
        None,
        true,
    )
    .unwrap();

    // An unsaved but valid account still renders as the sole current entry
    // when there are no saved profiles, in both list output formats.
    let current = token_set(
        "current@example.com",
        "Pro",
        Some("current-account"),
        Some("current-access"),
    );
    fs::write(
        &paths.auth,
        serde_json::json!({
            "tokens": {
                "account_id": current.account_id,
                "id_token": current.id_token,
                "access_token": current.access_token,
                "refresh_token": current.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    list_profiles(&paths, true, false).unwrap();
    list_profiles(&paths, false, false).unwrap();

    // A damaged profiles location produces a real load error, so the
    // selected-status dispatcher returns it instead of the no-profiles hint.
    let broken_dir = tempfile::tempdir().unwrap();
    let broken_paths = make_paths(broken_dir.path());
    fs::write(&broken_paths.profiles, "not a directory").unwrap();
    let error = status_profiles(
        &broken_paths,
        false,
        false,
        Some("missing".to_string()),
        None,
        false,
    )
    .unwrap_err();
    assert!(!error.is_empty());

    // The public dispatcher guarantees one selector, but retain coverage for
    // the defensive helper contract if it is called directly in this module.
    let selector_dir = tempfile::tempdir().unwrap();
    let selector_paths = make_paths(selector_dir.path());
    crate::ensure_paths(&selector_paths).unwrap();
    fs::write(
        &selector_paths.auth,
        serde_json::json!({"OPENAI_API_KEY": "sk-selector"}).to_string(),
    )
    .unwrap();
    save_profile(&selector_paths, Some("selector".to_string()), false).unwrap();
    let error = status_selected_profile(&selector_paths, None, None, false).unwrap_err();
    assert!(error.contains("status selector requires a label or id"));

    let context = ctx(dir.path(), false, false, false);
    let result = make_selected_current_entry(
        &paths,
        "missing",
        None,
        &Labels::new(),
        &BTreeMap::new(),
        &context,
    );
    let error = match result {
        Ok(_) => panic!("missing current profile unexpectedly succeeded"),
        Err(error) => error,
    };
    assert_eq!(error, AUTH_ERR_REFRESH_STATE_CHANGED);
}

#[test]
fn list_and_status_render_current_saved_and_selected_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).unwrap();
    let work = token_set(
        "work@example.com",
        "Pro",
        Some("account-work"),
        Some("work-access"),
    );
    fs::write(
        &paths.auth,
        serde_json::json!({
            "tokens": {
                "account_id": work.account_id,
                "id_token": work.id_token,
                "access_token": work.access_token,
                "refresh_token": work.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    save_profile(&paths, Some("work".to_string()), false).unwrap();

    let personal = token_set(
        "personal@example.com",
        "Pro",
        Some("account-personal"),
        Some("personal-access"),
    );
    fs::write(
        &paths.auth,
        serde_json::json!({
            "tokens": {
                "account_id": personal.account_id,
                "id_token": personal.id_token,
                "access_token": personal.access_token,
                "refresh_token": personal.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    save_profile(&paths, Some("personal".to_string()), false).unwrap();

    let snapshot = load_snapshot(&paths, false).unwrap();
    let ids: Vec<String> = snapshot.tokens.keys().cloned().collect();
    assert_eq!(ids.len(), 2);
    let current_id = current_saved_id(&paths, &snapshot.tokens).unwrap();
    let selected_id = ids.iter().find(|id| **id != current_id).cloned().unwrap();
    // Keep these renderer tests deterministic and offline while still driving
    // the account-profile usage/error paths through every status shape.
    fs::write(
        paths.codex.join("config.toml"),
        "chatgpt_base_url = \"https://example.com\"\n",
    )
    .unwrap();
    list_profiles(&paths, false, false).unwrap();
    list_profiles(&paths, true, false).unwrap();
    status_profiles(&paths, false, false, None, None, false).unwrap();
    status_profiles(&paths, false, false, None, None, true).unwrap();
    status_profiles(&paths, true, false, None, None, false).unwrap();
    status_profiles(&paths, true, true, None, None, false).unwrap();
    status_profiles(&paths, true, false, None, None, true).unwrap();
    status_profiles(&paths, false, false, None, Some(selected_id.clone()), false).unwrap();
    status_profiles(&paths, false, false, None, Some(selected_id), true).unwrap();
    status_profiles(&paths, false, false, None, Some(current_id.clone()), false).unwrap();
    status_profiles(&paths, false, false, None, Some(current_id), true).unwrap();
    assert!(loaded_profile_status(&paths).is_err());

    fs::write(&paths.auth, r#"{"tokens":{"account_id":"acct-broken"}}"#).unwrap();
    assert!(loaded_profile_status(&paths).is_err());
    list_profiles(&paths, false, false).unwrap();

    let active = token_set("active@example.com", "Pro", Some("active"), Some("access"));
    fs::write(
        &paths.auth,
        serde_json::json!({
            "tokens": {
                "account_id": active.account_id,
                "id_token": active.id_token,
                "access_token": active.access_token,
                "refresh_token": active.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    let mut stale_map = BTreeMap::new();
    stale_map.insert(
        "saved-other".to_string(),
        Ok(token_set(
            "other@example.com",
            "Pro",
            Some("other"),
            Some("access"),
        )),
    );
    let result = make_selected_current_entry(
        &paths,
        "saved-other",
        Some("saved-other"),
        &Labels::new(),
        &stale_map,
        &ctx(dir.path(), false, false, false),
    );
    let error = match result {
        Ok(_) => panic!("stale selected profile unexpectedly succeeded"),
        Err(error) => error,
    };
    assert_eq!(error, AUTH_ERR_REFRESH_STATE_CHANGED);
}

#[test]
fn status_all_direct_path_renders_current_and_saved_entries() {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).unwrap();

    let write_active = |account: &str, email: &str, access: &str| {
        let tokens = token_set(email, "Pro", Some(account), Some(access));
        fs::write(
            &paths.auth,
            serde_json::json!({
                "tokens": {
                    "account_id": tokens.account_id,
                    "id_token": tokens.id_token,
                    "access_token": tokens.access_token,
                    "refresh_token": tokens.refresh_token
                }
            })
            .to_string(),
        )
        .unwrap();
    };

    write_active("account-one", "one@example.com", "one-access");
    save_profile(&paths, Some("one".to_string()), false).unwrap();
    write_active("account-two", "two@example.com", "two-access");
    save_profile(&paths, Some("two".to_string()), false).unwrap();

    // Invalid usage configuration keeps this deterministic while still
    // building both the current entry and at least one saved entry.
    fs::write(
        paths.codex.join("config.toml"),
        "chatgpt_base_url = \"https://example.com\"\n",
    )
    .unwrap();
    status_all_profiles(&paths, false, false).unwrap();
}

#[test]
fn list_without_active_auth_renders_saved_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).unwrap();
    let tokens = token_set(
        "saved@example.com",
        "Pro",
        Some("saved-account"),
        Some("saved-access"),
    );
    fs::write(
        &paths.auth,
        serde_json::json!({
            "tokens": {
                "account_id": tokens.account_id,
                "id_token": tokens.id_token,
                "access_token": tokens.access_token,
                "refresh_token": tokens.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    save_profile(&paths, Some("saved".to_string()), false).unwrap();
    fs::remove_file(&paths.auth).unwrap();

    list_profiles(&paths, false, false).unwrap();
}

#[test]
fn status_selected_label_and_current_paths_render_with_valid_usage() {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).unwrap();
    let tokens = token_set(
        "label@example.com",
        "Pro",
        Some("label-account"),
        Some("label-access"),
    );
    fs::write(
        &paths.auth,
        serde_json::json!({
            "tokens": {
                "account_id": tokens.account_id,
                "id_token": tokens.id_token,
                "access_token": tokens.access_token,
                "refresh_token": tokens.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    save_profile(&paths, Some("label".to_string()), false).unwrap();

    let payload = r#"{"rate_limit":{"primary_window":{"used_percent":20.0,"limit_window_seconds":3600,"reset_at":1750000600}}}"#;
    let usage_url = spawn_usage_sequence(vec![
        http_ok_response(payload, "application/json"),
        http_ok_response(payload, "application/json"),
    ]);
    fs::write(
        paths.codex.join("config.toml"),
        format!("chatgpt_base_url = \"{usage_url}/backend-api\"\n"),
    )
    .unwrap();

    status_profiles(&paths, false, false, Some("label".to_string()), None, false).unwrap();
    status_profiles(
        &paths,
        false,
        false,
        None,
        Some("label@example.com-pro".to_string()),
        false,
    )
    .unwrap();
}

#[test]
fn status_selected_rejects_auth_change_between_current_reads() {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).unwrap();
    let tokens = token_set(
        "race@example.com",
        "Pro",
        Some("race-account"),
        Some("race-access"),
    );
    fs::write(
        &paths.auth,
        serde_json::json!({
            "tokens": {
                "account_id": tokens.account_id,
                "id_token": tokens.id_token,
                "access_token": tokens.access_token,
                "refresh_token": tokens.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    save_profile(&paths, Some("race".to_string()), false).unwrap();

    fs::write(
        paths.codex.join("config.toml"),
        "chatgpt_base_url = \"https://example.com\"\n",
    )
    .unwrap();
    FORCE_STATUS_SELECTED_AUTH_CHANGE.with(|force| force.set(true));
    let result = status_selected_profile(&paths, None, Some("race@example.com-pro"), false);
    assert_eq!(result.unwrap_err(), AUTH_ERR_REFRESH_STATE_CHANGED);
}

#[test]
fn status_all_valid_config_renders_current_entry_without_base_url_error() {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).unwrap();
    let tokens = token_set(
        "current@example.com",
        "Pro",
        Some("current-account"),
        Some("current-access"),
    );
    fs::write(
        &paths.auth,
        serde_json::json!({
            "tokens": {
                "account_id": tokens.account_id,
                "id_token": tokens.id_token,
                "access_token": tokens.access_token,
                "refresh_token": tokens.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    save_profile(&paths, Some("current".to_string()), false).unwrap();

    let payload = r#"{"rate_limit":{"primary_window":{"used_percent":20.0,"limit_window_seconds":3600,"reset_at":1750000600}}}"#;
    let usage_url = spawn_usage_sequence(vec![http_ok_response(payload, "application/json")]);
    fs::write(
        paths.codex.join("config.toml"),
        format!("chatgpt_base_url = \"{usage_url}/backend-api\"\n"),
    )
    .unwrap();
    status_all_profiles(&paths, false, false).unwrap();
}

#[test]
fn status_all_invalid_config_renders_saved_profiles_without_active_auth() {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).unwrap();
    let tokens = token_set(
        "saved@example.com",
        "Pro",
        Some("saved-account"),
        Some("saved-access"),
    );
    fs::write(
        &paths.auth,
        serde_json::json!({
            "tokens": {
                "account_id": tokens.account_id,
                "id_token": tokens.id_token,
                "access_token": tokens.access_token,
                "refresh_token": tokens.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    save_profile(&paths, Some("saved".to_string()), false).unwrap();
    fs::remove_file(&paths.auth).unwrap();
    fs::write(
        paths.codex.join("config.toml"),
        "chatgpt_base_url = \"https://example.com\"\n",
    )
    .unwrap();

    status_all_profiles(&paths, false, false).unwrap();
}

#[test]
fn selected_current_entry_reports_missing_active_file() {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).unwrap();
    let result = make_selected_current_entry(
        &paths,
        "missing",
        Some("missing"),
        &Labels::new(),
        &BTreeMap::new(),
        &ctx(dir.path(), false, false, false),
    );
    let error = match result {
        Ok(_) => panic!("missing active profile should fail"),
        Err(error) => error,
    };
    assert!(error.contains("not found"));
}

#[test]
fn usage_pool_and_compact_window_direct_paths_are_stable() {
    FORCE_USAGE_POOL_FALLBACK.with(|fallback| fallback.set(true));
    assert!(build_usage_pool(2).is_none());
    FORCE_USAGE_POOL_FALLBACK.with(|fallback| fallback.set(false));
    assert!(build_usage_pool(2).is_some());

    let context = ctx(Path::new("/tmp"), true, false, false);
    assert!(format_compact_window(&window(18_000, 80), context.now).contains("5h 80% left"));
    assert!(format_compact_window(&window(-1, 80), context.now).contains("unknown window"));
    let mut invalid_reset = window(18_000, 80);
    invalid_reset.reset_at = i64::MAX;
    assert!(format_compact_window(&invalid_reset, context.now).contains("resets unknown"));
}

#[test]
fn plain_error_lines_covers_status_merge_and_detail_styles() {
    let merged = plain_error_lines("request failed\nunexpected status 401\nbody", true);
    assert!(
        merged
            .iter()
            .any(|line| line.contains("unexpected status 401"))
    );
    assert!(merged.iter().any(|line| line.contains("body")));

    let plain = plain_error_lines("request failed\nbody", false);
    assert_eq!(plain, vec!["Error: request failed", "body"]);
    assert!(plain_error_lines("", false).is_empty());
}

#[test]
fn detail_lines_surfaces_failures_after_a_successful_refresh() {
    let _env_lock = ENV_MUTEX.lock().unwrap();
    for second_status in [401, 500] {
        let dir = tempfile::tempdir().unwrap();
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        let source = paths.profiles.join("profile.json");
        let initial = token_set(
            "user@example.com",
            "Pro",
            Some("account"),
            Some("old-access"),
        );
        fs::write(
            &source,
            serde_json::json!({
                "tokens": {
                    "account_id": initial.account_id,
                    "id_token": initial.id_token,
                    "access_token": initial.access_token,
                    "refresh_token": initial.refresh_token
                }
            })
            .to_string(),
        )
        .unwrap();

        let refreshed_id = build_id_token("user@example.com", "Pro");
        let refresh_url = spawn_server(http_ok_response(
            &format!(
                "{{\"access_token\":\"new-access\",\"id_token\":\"{refreshed_id}\",\"refresh_token\":\"new-refresh\"}}"
            ),
            "application/json",
        ));
        let first_error = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
        let second_error = format!("HTTP/1.1 {second_status} Failure\r\nContent-Length: 0\r\n\r\n");
        let usage_url = spawn_usage_sequence(vec![first_error.to_string(), second_error]);
        let _refresh_url = set_env_guard("CODEX_REFRESH_TOKEN_URL_OVERRIDE", Some(&refresh_url));

        let mut status_ctx = ctx(dir.path(), true, false, false);
        status_ctx.base_url = Some(usage_url);
        let result = detail_lines(
            &mut token_set(
                "user@example.com",
                "Pro",
                Some("account"),
                Some("old-access"),
            ),
            Some("user@example.com"),
            Some("Pro"),
            &status_ctx,
            &source,
        );
        assert_eq!(result.2.as_ref().unwrap().state, "error");
        assert!(result.1.is_some());
        assert!(!result.0.is_empty());
    }
}

#[test]
fn detail_lines_reports_second_usage_failure_after_refresh() {
    let _env_lock = ENV_MUTEX.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    fs::create_dir_all(&paths.profiles).unwrap();
    let source = paths.profiles.join("profile.json");
    let initial = token_set(
        "user@example.com",
        "Pro",
        Some("account"),
        Some("old-access"),
    );
    fs::write(
        &source,
        serde_json::json!({
            "tokens": {
                "account_id": initial.account_id,
                "id_token": initial.id_token,
                "access_token": initial.access_token,
                "refresh_token": initial.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    let refresh_url = spawn_server(http_ok_response(
        &format!(
            "{{\"access_token\":\"new-access\",\"id_token\":\"{}\",\"refresh_token\":\"new-refresh\"}}",
            build_id_token("user@example.com", "Pro")
        ),
        "application/json",
    ));
    let usage_url = spawn_usage_sequence(vec![
        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_string(),
        "HTTP/1.1 500 Failure\r\nContent-Length: 0\r\n\r\n".to_string(),
    ]);
    let _refresh_url = set_env_guard("CODEX_REFRESH_TOKEN_URL_OVERRIDE", Some(&refresh_url));
    let mut status_ctx = ctx(dir.path(), true, false, false);
    status_ctx.base_url = Some(usage_url);
    let result = detail_lines(
        &mut token_set(
            "user@example.com",
            "Pro",
            Some("account"),
            Some("old-access"),
        ),
        Some("user@example.com"),
        Some("Pro"),
        &status_ctx,
        &source,
    );
    assert_eq!(result.2.as_ref().unwrap().state, "error");
    assert!(!result.0.is_empty());
}

#[test]
fn detail_lines_reports_initial_non_auth_usage_failure() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("profile.json");
    let response = "HTTP/1.1 402 Payment Required\r\nContent-Length: 0\r\n\r\n";
    let usage_url = spawn_usage_sequence(vec![response.to_string()]);
    let mut status_ctx = ctx(dir.path(), true, false, false);
    status_ctx.base_url = Some(usage_url);
    let mut account = token_set("user@example.com", "Pro", Some("account"), Some("access"));
    let result = detail_lines(
        &mut account,
        Some("user@example.com"),
        Some("Pro"),
        &status_ctx,
        &source,
    );
    assert_eq!(result.2.as_ref().unwrap().state, "error");
    assert!(result.1.is_some());
    assert!(!result.0.is_empty());
}

#[test]
fn make_current_reports_sync_failure_after_status_refresh() {
    let _env_lock = ENV_MUTEX.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).unwrap();
    let initial = token_set(
        "user@example.com",
        "Pro",
        Some("account"),
        Some("old-access"),
    );
    fs::write(
        &paths.auth,
        serde_json::json!({
            "tokens": {
                "account_id": initial.account_id,
                "id_token": initial.id_token,
                "access_token": initial.access_token,
                "refresh_token": initial.refresh_token
            }
        })
        .to_string(),
    )
    .unwrap();
    let refresh_id = build_id_token("user@example.com", "Pro");
    let refresh_url = spawn_server(http_ok_response(
        &format!(
            "{{\"access_token\":\"new-access\",\"id_token\":\"{refresh_id}\",\"refresh_token\":\"new-refresh\"}}"
        ),
        "application/json",
    ));
    let usage_url = spawn_usage_sequence(vec![
        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_string(),
        http_ok_response(
            r#"{"rate_limit":{"primary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":2000000000}}}"#,
            "application/json",
        ),
    ]);
    let _refresh_override = set_env_guard("CODEX_REFRESH_TOKEN_URL_OVERRIDE", Some(&refresh_url));
    let mut status_ctx = ctx(dir.path(), true, false, false);
    status_ctx.base_url = Some(usage_url);
    let mut tokens_map = BTreeMap::new();
    tokens_map.insert("saved".to_string(), Ok(initial));
    let entry = make_current(
        &paths,
        Some("saved"),
        &Labels::new(),
        &tokens_map,
        &status_ctx,
    )
    .unwrap();
    assert!(entry.error_summary.unwrap().contains("Error"));
    assert_eq!(entry.usage.unwrap().state, "error");
}

#[test]
fn prompt_and_profile_file_helpers_cover_non_cancel_errors() {
    let prompt = confirm_delete_profiles_with(
        true,
        Err(inquire::error::InquireError::InvalidConfiguration(
            "invalid delete prompt".to_string(),
        )),
    )
    .unwrap_err();
    assert!(prompt.contains("Could not prompt for delete"));
    let candidate = Candidate {
        id: "id".to_string(),
        display: "Candidate".to_string(),
    };
    assert_eq!(candidate.to_string(), "Candidate");
    let prompt_error = handle_inquire_result::<()>(
        Err(inquire::error::InquireError::InvalidConfiguration(
            "invalid load prompt".to_string(),
        )),
        "load profile",
    )
    .unwrap_err();
    assert!(prompt_error.contains("load profile"));

    assert!(is_profile_file(Path::new("profile.json")));
    assert!(!is_profile_file(Path::new("profiles.json")));
    assert!(!is_profile_file(Path::new("update.json")));
    assert!(!is_profile_file(Path::new("profile")));
    assert!(!is_profile_file(Path::new("profile.toml")));
}

#[test]
fn list_context_reads_valid_and_invalid_base_urls() {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    fs::create_dir_all(&paths.codex).unwrap();
    let no_usage = ListCtx::new(&paths, false, false, false);
    assert!(no_usage.base_url.is_none());

    fs::write(
        paths.codex.join("config.toml"),
        "chatgpt_base_url = \"http://127.0.0.1:9999\"\n",
    )
    .unwrap();
    let valid = ListCtx::new(&paths, true, true, true);
    assert_eq!(valid.base_url.as_deref(), Some("http://127.0.0.1:9999"));

    fs::write(
        paths.codex.join("config.toml"),
        "chatgpt_base_url = \"https://evil.example/?token=secret\"\n",
    )
    .unwrap();
    let invalid = ListCtx::new(&paths, true, false, false);
    assert!(invalid.base_url.is_none());
    assert!(invalid.base_url_error.is_some());
}
