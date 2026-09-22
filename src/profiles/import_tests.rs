use super::*;
use crate::test_utils::{build_id_token, make_paths};
use serde_json::{Value, json};
use std::fs;

fn oauth_contents() -> Value {
    json!({
        "tokens": {
            "account_id": "oauth-account",
            "id_token": build_id_token("oauth@example.com", "team"),
            "access_token": "oauth-access",
            "refresh_token": "oauth-refresh"
        }
    })
}

fn import_one(contents: Value) -> (tempfile::TempDir, Paths, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).expect("ensure paths");
    let id = "imported@example.com-team".to_string();
    let input = dir.path().join("bundle.json");
    let bundle = json!({
        "version": 1,
        "profiles": [{
            "id": id,
            "contents": contents
        }]
    });
    fs::write(
        &input,
        serde_json::to_vec(&bundle).expect("serialize bundle"),
    )
    .expect("write bundle");
    import_profiles(&paths, input, false).expect("import profile");
    (dir, paths, id)
}

fn assert_import_rejected(contents: Value, expected: &str) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).expect("ensure paths");
    let input = dir.path().join("bundle.json");
    let id = "rejected@example.com-team";
    let bundle = json!({
        "version": 1,
        "profiles": [{
            "id": id,
            "contents": contents
        }]
    });
    fs::write(
        &input,
        serde_json::to_vec(&bundle).expect("serialize bundle"),
    )
    .expect("write bundle");

    let error = import_profiles(&paths, input, false).expect_err("import should fail");
    assert!(error.contains(expected), "{error}");
    assert!(!profile_path_for_id(&paths.profiles, id).exists());
    assert!(!paths.profiles_index.exists());
}

fn profile_path(paths: &Paths, id: &str) -> std::path::PathBuf {
    profile_path_for_id(&paths.profiles, id)
}

#[test]
fn import_uses_codex_auth_mode_precedence_for_mixed_credentials() {
    let mut default_mixed = oauth_contents();
    default_mixed["OPENAI_API_KEY"] = json!("sk-default");
    let (_dir, paths, id) = import_one(default_mixed);
    let tokens = read_tokens(&profile_path(&paths, &id)).expect("resolved default credentials");
    assert!(is_api_key_profile(&tokens));
    assert!(read_profiles_index(&paths).unwrap().profiles[&id].is_api_key);

    let mut explicit_api_key = oauth_contents();
    explicit_api_key["auth_mode"] = json!("apikey");
    explicit_api_key["OPENAI_API_KEY"] = json!("sk-explicit");
    let (_dir, paths, id) = import_one(explicit_api_key);
    let tokens = read_tokens(&profile_path(&paths, &id)).expect("resolved API-key credentials");
    assert!(is_api_key_profile(&tokens));
    assert!(read_profiles_index(&paths).unwrap().profiles[&id].is_api_key);

    let mut explicit_chatgpt = oauth_contents();
    explicit_chatgpt["auth_mode"] = json!("chatgpt");
    explicit_chatgpt["OPENAI_API_KEY"] = json!("sk-ignored");
    let (_dir, paths, id) = import_one(explicit_chatgpt);
    let tokens = read_tokens(&profile_path(&paths, &id)).expect("resolved OAuth credentials");
    assert!(!is_api_key_profile(&tokens));
    assert_eq!(tokens.account_id.as_deref(), Some("oauth-account"));
    assert!(!read_profiles_index(&paths).unwrap().profiles[&id].is_api_key);
}

#[test]
fn import_rejects_modes_the_active_reader_would_reject_before_writing() {
    let mut unsupported = oauth_contents();
    unsupported["auth_mode"] = json!("personalAccessToken");
    assert_import_rejected(unsupported, "Unsupported authentication mode");

    let mut missing_key = oauth_contents();
    missing_key["auth_mode"] = json!("apikey");
    assert_import_rejected(missing_key, "API-key authentication has no OPENAI_API_KEY");

    let mut invalid_mode = oauth_contents();
    invalid_mode["auth_mode"] = json!(42);
    assert_import_rejected(invalid_mode, "Invalid auth_mode: expected a string");
}
