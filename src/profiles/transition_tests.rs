use super::*;
use crate::test_utils::{build_id_token, make_paths};
use std::fs;

fn setup() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    crate::ensure_paths(&paths).unwrap();
    (dir, paths)
}

fn auth_value(account: &str, email: &str, access: &str, refresh: &str) -> serde_json::Value {
    serde_json::json!({
        "tokens": {
            "account_id": account,
            "id_token": build_id_token(email, "plus"),
            "access_token": access,
            "refresh_token": refresh
        }
    })
}

fn write_auth(paths: &Paths, account: &str, email: &str, access: &str, refresh: &str) {
    fs::write(
        &paths.auth,
        serde_json::to_string(&auth_value(account, email, access, refresh)).unwrap(),
    )
    .unwrap();
}

fn write_auth_with_last_refresh(
    paths: &Paths,
    account: &str,
    email: &str,
    access: &str,
    refresh: &str,
    last_refresh: &str,
) {
    let mut value = auth_value(account, email, access, refresh);
    value["last_refresh"] = serde_json::Value::String(last_refresh.to_string());
    fs::write(&paths.auth, serde_json::to_string(&value).unwrap()).unwrap();
}

fn write_profile(paths: &Paths, id: &str, account: &str, email: &str, access: &str) {
    fs::write(
        profile_path_for_id(&paths.profiles, id),
        serde_json::to_string(&auth_value(account, email, access, "saved-refresh")).unwrap(),
    )
    .unwrap();
}

fn write_index(paths: &Paths, entries: &[(&str, &str)]) {
    let mut index = ProfilesIndex::default();
    for (id, label) in entries {
        index.profiles.insert(
            (*id).to_string(),
            ProfileIndexEntry {
                label: Some((*label).to_string()),
                ..Default::default()
            },
        );
    }
    write_profiles_index(paths, &index).unwrap();
}

fn import_bundle(paths: &Paths, profiles: serde_json::Value) -> PathBuf {
    let path = paths.codex.join("import.json");
    fs::write(
        &path,
        serde_json::json!({"version": 1, "profiles": profiles}).to_string(),
    )
    .unwrap();
    path
}

#[test]
fn load_preserves_duplicate_identity_aliases_and_labels() {
    let (_dir, paths) = setup();
    write_auth(
        &paths,
        "same-account",
        "alias@example.com",
        "saved-b",
        "saved-refresh",
    );
    write_profile(
        &paths,
        "alias-a",
        "same-account",
        "alias@example.com",
        "saved-a",
    );
    write_profile(
        &paths,
        "alias-b",
        "same-account",
        "alias@example.com",
        "saved-b",
    );
    write_index(&paths, &[("alias-a", "Alias A"), ("alias-b", "Alias B")]);

    load_profile(&paths, Some("Alias A".to_string()), None, true, false, true).unwrap();

    assert!(profile_path_for_id(&paths.profiles, "alias-a").is_file());
    assert!(profile_path_for_id(&paths.profiles, "alias-b").is_file());
    assert!(!profile_path_for_id(&paths.profiles, "alias@example.com-plus").exists());
    let index = read_profiles_index(&paths).unwrap();
    assert_eq!(index.profiles["alias-a"].label.as_deref(), Some("Alias A"));
    assert_eq!(index.profiles["alias-b"].label.as_deref(), Some("Alias B"));
    assert_eq!(
        read_tokens(&paths.auth).unwrap().access_token.as_deref(),
        Some("saved-a")
    );
}

#[test]
fn load_preserves_selected_duplicate_alias_when_another_alias_is_active() {
    let (_dir, paths) = setup();
    write_auth(
        &paths,
        "same-account",
        "alias@example.com",
        "active-b",
        "saved-refresh",
    );
    write_profile(
        &paths,
        "alias-a",
        "same-account",
        "alias@example.com",
        "saved-a",
    );
    write_profile(
        &paths,
        "alias-b",
        "same-account",
        "alias@example.com",
        "active-b",
    );
    write_index(&paths, &[("alias-a", "Alias A"), ("alias-b", "Alias B")]);

    load_profile(&paths, None, Some("alias-a".to_string()), true, false, true).unwrap();

    assert_eq!(
        read_tokens(&paths.auth).unwrap().access_token.as_deref(),
        Some("saved-a")
    );
    assert_eq!(
        read_tokens(&profile_path_for_id(&paths.profiles, "alias-a"))
            .unwrap()
            .access_token
            .as_deref(),
        Some("saved-a")
    );
    assert_eq!(
        read_tokens(&profile_path_for_id(&paths.profiles, "alias-b"))
            .unwrap()
            .access_token
            .as_deref(),
        Some("active-b")
    );
}

#[test]
fn load_rejects_ambiguous_duplicate_aliases_without_exact_active_tokens() {
    let (_dir, paths) = setup();
    write_auth(
        &paths,
        "same-account",
        "alias@example.com",
        "rotated-active",
        "rotated-refresh",
    );
    write_profile(
        &paths,
        "alias-a",
        "same-account",
        "alias@example.com",
        "saved-a",
    );
    write_profile(
        &paths,
        "alias-b",
        "same-account",
        "alias@example.com",
        "saved-b",
    );
    write_index(&paths, &[("alias-a", "Alias A"), ("alias-b", "Alias B")]);
    let before_auth = fs::read(&paths.auth).unwrap();
    let before_a = fs::read(profile_path_for_id(&paths.profiles, "alias-a")).unwrap();
    let before_b = fs::read(profile_path_for_id(&paths.profiles, "alias-b")).unwrap();

    let error =
        load_profile(&paths, None, Some("alias-a".to_string()), true, false, true).unwrap_err();
    assert_eq!(error, AUTH_ERR_AMBIGUOUS_SAVED_PROFILES);
    assert_eq!(fs::read(&paths.auth).unwrap(), before_auth);
    assert_eq!(
        fs::read(profile_path_for_id(&paths.profiles, "alias-a")).unwrap(),
        before_a
    );
    assert_eq!(
        fs::read(profile_path_for_id(&paths.profiles, "alias-b")).unwrap(),
        before_b
    );
}

#[test]
fn load_syncs_a_rotated_active_single_profile_before_switching() {
    let (_dir, paths) = setup();
    write_auth(
        &paths,
        "same-account",
        "one@example.com",
        "rotated-active",
        "rotated-refresh",
    );
    write_profile(
        &paths,
        "only",
        "same-account",
        "one@example.com",
        "saved-access",
    );
    write_index(&paths, &[("only", "Only")]);

    load_profile(&paths, None, Some("only".to_string()), true, false, true).unwrap();

    assert_eq!(
        read_tokens(&profile_path_for_id(&paths.profiles, "only"))
            .unwrap()
            .access_token
            .as_deref(),
        Some("rotated-active")
    );
    assert_eq!(
        read_tokens(&paths.auth).unwrap().access_token.as_deref(),
        Some("rotated-active")
    );
}

#[test]
fn load_preserves_active_auth_metadata_when_tokens_match_selected_profile() {
    let (_dir, paths) = setup();
    write_auth_with_last_refresh(
        &paths,
        "same-account",
        "same@example.com",
        "same-access",
        "same-refresh",
        "active-metadata",
    );
    let mut saved = auth_value(
        "same-account",
        "same@example.com",
        "same-access",
        "same-refresh",
    );
    saved["last_refresh"] = serde_json::Value::String("saved-metadata".to_string());
    fs::write(
        profile_path_for_id(&paths.profiles, "same"),
        serde_json::to_string(&saved).unwrap(),
    )
    .unwrap();
    write_index(&paths, &[("same", "Same")]);

    load_profile(&paths, None, Some("same".to_string()), true, false, true).unwrap();

    let auth: serde_json::Value = serde_json::from_slice(&fs::read(&paths.auth).unwrap()).unwrap();
    assert_eq!(auth["last_refresh"], "active-metadata");
}

#[test]
fn status_resolves_duplicate_alias_from_exact_active_tokens() {
    let (_dir, paths) = setup();
    write_auth(
        &paths,
        "same-account",
        "alias@example.com",
        "active-b",
        "saved-refresh",
    );
    write_profile(
        &paths,
        "alias-a",
        "same-account",
        "alias@example.com",
        "saved-a",
    );
    write_profile(
        &paths,
        "alias-b",
        "same-account",
        "alias@example.com",
        "active-b",
    );
    write_index(&paths, &[("alias-a", "Alias A"), ("alias-b", "Alias B")]);

    let snapshot = load_snapshot(&paths, true).unwrap();
    assert_eq!(
        current_saved_id(&paths, &snapshot.tokens).as_deref(),
        Some("alias-b")
    );
    let ctx = ListCtx::new(&paths, false, true, false);
    let entry = make_current(
        &paths,
        Some("alias-a"),
        &snapshot.labels,
        &snapshot.tokens,
        &ctx,
    )
    .unwrap();
    assert_eq!(entry.id.as_deref(), Some("alias-b"));
}

#[test]
fn incomplete_active_auth_does_not_resolve_to_a_saved_alias() {
    let (_dir, paths) = setup();
    write_profile(&paths, "saved", "account", "saved@example.com", "access");
    let saved = load_profile_tokens_map(&paths).unwrap();
    let incomplete = Tokens {
        account_id: None,
        id_token: None,
        access_token: Some("incomplete-access".to_string()),
        refresh_token: None,
    };
    assert!(pick_cached_profile_id(&saved, &incomplete).is_none());
    assert!(!cached_profile_id_is_ambiguous(&saved, &incomplete));
}

#[test]
fn load_json_switches_to_profile_from_another_identity() {
    let (_dir, paths) = setup();
    write_auth(
        &paths,
        "current-account",
        "current@example.com",
        "current-access",
        "current-refresh",
    );
    write_profile(
        &paths,
        "saved",
        "saved-account",
        "saved@example.com",
        "saved-access",
    );
    write_index(&paths, &[("saved", "Saved")]);
    fs::write(
        paths.codex.join("config.toml"),
        "chatgpt_base_url = 'ftp://localhost'\n",
    )
    .unwrap();

    load_profile(&paths, None, Some("saved".to_string()), true, true, true).unwrap();

    assert_eq!(
        read_tokens(&paths.auth).unwrap().access_token.as_deref(),
        Some("saved-access")
    );
}

#[test]
fn load_rejects_invalid_selected_profile() {
    let (_dir, paths) = setup();
    write_auth(
        &paths,
        "current-account",
        "current@example.com",
        "current-access",
        "current-refresh",
    );
    fs::write(
        profile_path_for_id(&paths.profiles, "broken"),
        r#"{"unexpected":true}"#,
    )
    .unwrap();
    write_index(&paths, &[("broken", "Broken")]);

    let err =
        load_profile(&paths, None, Some("broken".to_string()), true, false, false).unwrap_err();
    assert!(err.contains("Selected profile is invalid"));
    assert_eq!(
        read_tokens(&paths.auth).unwrap().access_token.as_deref(),
        Some("current-access")
    );
}

#[test]
fn load_and_delete_report_no_profiles_without_tty() {
    let (_dir, paths) = setup();
    assert!(load_profile(&paths, None, None, true, false, false).is_err());
    delete_profile(&paths, true, None, Vec::new(), false).unwrap();
}

#[test]
fn delete_profile_plain_and_json_remove_selected_files() {
    let (_dir, paths) = setup();
    write_profile(
        &paths,
        "one",
        "one-account",
        "one@example.com",
        "one-access",
    );
    write_profile(
        &paths,
        "two",
        "two-account",
        "two@example.com",
        "two-access",
    );
    write_index(&paths, &[("one", "One"), ("two", "Two")]);

    delete_profile(&paths, true, None, vec!["one".to_string()], false).unwrap();
    assert!(!profile_path_for_id(&paths.profiles, "one").exists());
    assert!(profile_path_for_id(&paths.profiles, "two").exists());

    delete_profile(&paths, true, None, vec!["two".to_string()], true).unwrap();
    assert!(!profile_path_for_id(&paths.profiles, "two").exists());
    assert!(read_profiles_index(&paths).unwrap().profiles.is_empty());
}

#[test]
fn delete_profile_rejects_unknown_id() {
    let (_dir, paths) = setup();
    write_profile(
        &paths,
        "one",
        "one-account",
        "one@example.com",
        "one-access",
    );
    write_index(&paths, &[("one", "One")]);

    let err = delete_profile(&paths, true, None, vec!["missing".to_string()], false).unwrap_err();
    assert!(err.contains("missing"));
    assert!(profile_path_for_id(&paths.profiles, "one").exists());
}

#[test]
fn import_rejects_reserved_ids_case_insensitively() {
    let (_dir, paths) = setup();
    write_profiles_index(&paths, &ProfilesIndex::default()).unwrap();
    let before = fs::read(&paths.profiles_index).unwrap();
    for id in ["Profiles", "pRoFiLeS", "UPDATE", "uPdAtE"] {
        let input = import_bundle(
            &paths,
            serde_json::json!([{"id": id, "contents": auth_value("account", "import@example.com", "access", "refresh")}]),
        );
        let error = import_profiles(&paths, input, false).unwrap_err();
        assert!(error.contains("reserved"), "{error}");
        assert_eq!(fs::read(&paths.profiles_index).unwrap(), before);
        assert!(collect_profile_ids(&paths.profiles).unwrap().is_empty());
    }
    assert!(!is_profile_file(Path::new("Profiles.json")));
    assert!(!is_profile_file(Path::new("UPDATE.JSON")));
}

#[test]
fn import_rejects_case_and_unicode_case_duplicate_ids_before_writing() {
    let (_dir, paths) = setup();
    for ids in [["work", "WORK"], ["Équipe", "équipe"]] {
        let entries: Vec<_> = ids
            .iter()
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "contents": auth_value("account", "import@example.com", "access", "refresh")
                })
            })
            .collect();
        let input = import_bundle(&paths, serde_json::Value::Array(entries));
        let error = import_profiles(&paths, input, false).unwrap_err();
        assert!(error.contains("duplicate"), "{error}");
        assert!(collect_profile_ids(&paths.profiles).unwrap().is_empty());
    }
}

#[cfg(windows)]
#[test]
fn import_rejects_case_insensitive_existing_profile_without_mutation() {
    let (_dir, paths) = setup();
    write_profile(
        &paths,
        "work",
        "existing-account",
        "existing@example.com",
        "existing-access",
    );
    write_index(&paths, &[("work", "Work")]);
    let existing_path = profile_path_for_id(&paths.profiles, "work");
    let before_profile = fs::read(&existing_path).unwrap();
    let before_index = fs::read(&paths.profiles_index).unwrap();
    let input = import_bundle(
        &paths,
        serde_json::json!([{"id": "WORK", "contents": auth_value("replacement-account", "replacement@example.com", "new-access", "new-refresh")}]),
    );

    let error = import_profiles(&paths, input, false).unwrap_err();
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(fs::read(existing_path).unwrap(), before_profile);
    assert_eq!(fs::read(&paths.profiles_index).unwrap(), before_index);
}
