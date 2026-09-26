use super::*;
use crate::test_utils::{build_id_token, make_paths};

fn auth(account: &str) -> serde_json::Value {
    serde_json::json!({"tokens": {
        "account_id": account,
        "id_token": build_id_token("storage@example.com", "plus"),
        "access_token": "synthetic-access",
        "refresh_token": "synthetic-refresh"
    }})
}

fn setup() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = make_paths(dir.path());
    fs::create_dir_all(&paths.profiles).unwrap();
    (dir, paths)
}

fn write_profile(paths: &Paths, id: &str, account: &str) -> Tokens {
    let file = profile_path_for_id(&paths.profiles, id);
    fs::write(&file, auth(account).to_string()).unwrap();
    read_tokens(&file).unwrap()
}

fn bundle(paths: &Paths, profiles: serde_json::Value) -> PathBuf {
    let file = paths.codex.join("bundle.json");
    fs::write(
        &file,
        serde_json::json!({"version":1,"profiles":profiles}).to_string(),
    )
    .unwrap();
    file
}

#[test]
fn import_rejects_invalid_input_without_creating_profiles() {
    let (_dir, paths) = setup();
    assert!(
        import_profiles(&paths, paths.codex.join("missing"), false)
            .unwrap_err()
            .contains("Could not read")
    );
    let file = bundle(&paths, serde_json::json!([]));
    fs::write(&file, "{").unwrap();
    assert!(
        import_profiles(&paths, file.clone(), false)
            .unwrap_err()
            .contains("invalid JSON")
    );
    fs::write(&file, r#"{"version":2,"profiles":[]}"#).unwrap();
    assert!(
        import_profiles(&paths, file, false)
            .unwrap_err()
            .contains("not supported")
    );
    for (contents, expected) in [
        (serde_json::json!({"tokens":42}), "invalid JSON"),
        (serde_json::json!({}), "missing tokens"),
        (
            serde_json::json!({"tokens":{"access_token":"only"}}),
            "incomplete",
        ),
    ] {
        let file = bundle(
            &paths,
            serde_json::json!([{"id":"new","contents":contents}]),
        );
        assert!(
            import_profiles(&paths, file, false)
                .unwrap_err()
                .contains(expected)
        );
        assert!(!profile_path_for_id(&paths.profiles, "new").exists());
    }
}

#[test]
fn import_validates_entire_bundle_before_writing() {
    let (_dir, paths) = setup();
    for ids in [
        ["duplicate", "duplicate"],
        ["first", "../outside"],
        ["first", "profiles"],
        ["first", "update"],
    ] {
        let entries: Vec<_> = ids
            .iter()
            .map(|id| serde_json::json!({"id":id,"contents":auth("account")}))
            .collect();
        let input = bundle(&paths, serde_json::json!(entries));
        assert!(import_profiles(&paths, input, false).is_err());
        assert!(collect_profile_ids(&paths.profiles).unwrap().is_empty());
    }
    write_profile(&paths, "existing", "existing-account");
    let before = fs::read(profile_path_for_id(&paths.profiles, "existing")).unwrap();
    let input = bundle(
        &paths,
        serde_json::json!([{"id":"existing","contents":auth("replacement")} ]),
    );
    assert!(
        import_profiles(&paths, input, false)
            .unwrap_err()
            .contains("already exists")
    );
    assert_eq!(
        fs::read(profile_path_for_id(&paths.profiles, "existing")).unwrap(),
        before
    );
}

#[test]
fn import_rolls_back_profiles_when_a_later_atomic_write_fails() {
    use crate::common::{FAIL_WRITE_RENAME, FailpointGuard};
    for failed_write in [2, 3] {
        let (_dir, paths) = setup();
        write_profile(&paths, "existing", "keep");
        let mut index = ProfilesIndex::default();
        index
            .profiles
            .insert("existing".into(), ProfileIndexEntry::default());
        write_profiles_index(&paths, &index).unwrap();
        let before = fs::read(&paths.profiles_index).unwrap();
        let input = bundle(
            &paths,
            serde_json::json!([
                {"id":"first","label":"First","contents":auth("first")},
                {"id":"second","label":"Second","contents":auth("second")}
            ]),
        );
        {
            let _failure = FailpointGuard::new(FAIL_WRITE_RENAME, failed_write);
            assert!(import_profiles(&paths, input, false).is_err());
        }
        assert!(!paths.profiles.join("first.json").exists());
        assert!(!paths.profiles.join("second.json").exists());
        assert!(paths.profiles.join("existing.json").exists());
        assert_eq!(fs::read(&paths.profiles_index).unwrap(), before);
        assert!(!fs::read_dir(&paths.profiles).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp")
        }));
    }
}

#[test]
fn import_preserves_a_destination_created_after_validation() {
    use crate::common::AtomicCommitHookGuard;

    let (_dir, paths) = setup();
    write_profiles_index(&paths, &ProfilesIndex::default()).unwrap();
    let index_before = fs::read(&paths.profiles_index).unwrap();
    let input = bundle(
        &paths,
        serde_json::json!([
            {"id":"first","label":"First","contents":auth("first")},
            {"id":"second","label":"Second","contents":auth("second")}
        ]),
    );
    let _hook = AtomicCommitHookGuard::on_nth_commit(
        |_, destination| {
            assert_eq!(destination.file_name().unwrap(), "second.json");
            assert!(!destination.exists());
            fs::write(destination, b"synthetic competing profile").unwrap();
        },
        2,
    );

    let result = import_profiles(&paths, input, false);
    assert_eq!(
        fs::read(paths.profiles.join("second.json")).unwrap(),
        b"synthetic competing profile"
    );
    assert_eq!(
        result.unwrap_err(),
        "Error: Profile 'second' already exists."
    );
    assert!(!paths.profiles.join("first.json").exists());
    assert_eq!(fs::read(&paths.profiles_index).unwrap(), index_before);
}

#[test]
fn import_rollback_preserves_another_writers_identical_replacement() {
    use crate::common::{AtomicCommitHookGuard, FAIL_WRITE_RENAME, FailpointGuard};

    for failed_write in [2, 3] {
        let (_dir, paths) = setup();
        write_profiles_index(&paths, &ProfilesIndex::default()).unwrap();
        let index_before = fs::read(&paths.profiles_index).unwrap();
        let input = bundle(
            &paths,
            serde_json::json!([
                {"id":"first","contents":auth("first")},
                {"id":"second","contents":auth("second")}
            ]),
        );
        let _hook = AtomicCommitHookGuard::on_nth_commit(
            |_, destination| {
                let first = destination.with_file_name("first.json");
                let replacement = destination.with_file_name("replacement");
                fs::write(&replacement, fs::read(&first).unwrap()).unwrap();
                fs::rename(replacement, &first).unwrap();
            },
            failed_write,
        );
        let _failure = FailpointGuard::new(FAIL_WRITE_RENAME, failed_write);

        let result = import_profiles(&paths, input, false);
        assert!(
            paths.profiles.join("first.json").is_file(),
            "rollback removed another writer's replacement"
        );
        assert!(
            result
                .unwrap_err()
                .contains("Import rollback incomplete: 1")
        );
        assert!(!paths.profiles.join("second.json").exists());
        assert_eq!(fs::read(&paths.profiles_index).unwrap(), index_before);
    }
}

#[test]
fn import_rollback_preserves_an_in_place_edit() {
    use crate::common::{AtomicCommitHookGuard, FAIL_WRITE_RENAME, FailpointGuard};

    let (_dir, paths) = setup();
    write_profiles_index(&paths, &ProfilesIndex::default()).unwrap();
    let index_before = fs::read(&paths.profiles_index).unwrap();
    let input = bundle(
        &paths,
        serde_json::json!([
            {"id":"first","contents":auth("first")},
            {"id":"second","contents":auth("second")}
        ]),
    );
    let _hook = AtomicCommitHookGuard::on_nth_commit(
        |_, destination| {
            let first = destination.with_file_name("first.json");
            let original = fs::read(&first).unwrap();
            // Same inode and length: identity and metadata alone do not detect this.
            fs::write(first, vec![b'x'; original.len()]).unwrap();
        },
        2,
    );
    let _failure = FailpointGuard::new(FAIL_WRITE_RENAME, 2);

    let error = import_profiles(&paths, input, false).unwrap_err();
    assert!(error.contains("Import rollback incomplete: 1"));
    assert!(
        fs::read(paths.profiles.join("first.json"))
            .unwrap()
            .iter()
            .all(|byte| *byte == b'x')
    );
    assert!(!paths.profiles.join("second.json").exists());
    assert_eq!(fs::read(&paths.profiles_index).unwrap(), index_before);
}

#[cfg(unix)]
#[test]
fn import_rollback_preserves_a_symlink_replacement_and_its_target() {
    use crate::common::{AtomicCommitHookGuard, FAIL_WRITE_RENAME, FailpointGuard};
    use std::os::unix::fs::symlink;

    let (_dir, paths) = setup();
    write_profiles_index(&paths, &ProfilesIndex::default()).unwrap();
    let index_before = fs::read(&paths.profiles_index).unwrap();
    let input = bundle(
        &paths,
        serde_json::json!([
            {"id":"first","contents":auth("first")},
            {"id":"second","contents":auth("second")}
        ]),
    );
    let _hook = AtomicCommitHookGuard::on_nth_commit(
        |_, destination| {
            let first = destination.with_file_name("first.json");
            let target = destination.with_file_name("other-writer");
            fs::rename(&first, &target).unwrap();
            symlink("other-writer", first).unwrap();
        },
        2,
    );
    let _failure = FailpointGuard::new(FAIL_WRITE_RENAME, 2);

    let error = import_profiles(&paths, input, false).unwrap_err();
    assert!(error.contains("Import rollback incomplete: 1"));
    assert_eq!(
        fs::read_link(paths.profiles.join("first.json")).unwrap(),
        Path::new("other-writer")
    );
    assert!(paths.profiles.join("other-writer").is_file());
    assert!(!paths.profiles.join("second.json").exists());
    assert_eq!(fs::read(&paths.profiles_index).unwrap(), index_before);
}

#[test]
fn import_identity_failures_preserve_unknown_files_and_the_index() {
    use crate::common::{FAIL_FILE_IDENTITY, FailpointGuard};

    for failed_identity in [1, 2] {
        let (_dir, paths) = setup();
        write_profiles_index(&paths, &ProfilesIndex::default()).unwrap();
        let index_before = fs::read(&paths.profiles_index).unwrap();
        let input = bundle(
            &paths,
            serde_json::json!([
                {"id":"first","contents":auth("first")},
                {"id":"second","contents":auth("second")}
            ]),
        );
        let _failure = FailpointGuard::new(FAIL_FILE_IDENTITY, failed_identity);

        let error = import_profiles(&paths, input, false).unwrap_err();
        assert_eq!(
            paths.profiles.join("first.json").exists(),
            failed_identity == 2
        );
        assert_eq!(
            error.contains("Import rollback incomplete"),
            failed_identity == 2
        );
        assert!(!paths.profiles.join("second.json").exists());
        assert_eq!(fs::read(&paths.profiles_index).unwrap(), index_before);
        assert!(!fs::read_dir(&paths.profiles).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        }));
    }
}

#[test]
fn import_and_export_preserve_labels_and_api_keys() {
    let (_dir, paths) = setup();
    let input = bundle(
        &paths,
        serde_json::json!([
            {"id":"chat","label":"Work","contents":auth("work")},
            {"id":"api","contents":{"OPENAI_API_KEY":"synthetic-key"}}
        ]),
    );
    import_profiles(&paths, input, true).unwrap();
    assert_eq!(
        read_profiles_index(&paths).unwrap().profiles["chat"]
            .label
            .as_deref(),
        Some("Work")
    );
    let output = paths.codex.join("export.json");
    export_profiles(
        &paths,
        None,
        vec!["chat".into(), "chat".into(), "api".into()],
        output.clone(),
        true,
    )
    .unwrap();
    let exported: ExportBundle = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(exported.profiles.len(), 2);
    assert_eq!(exported.profiles[0].label.as_deref(), Some("Work"));
    assert_eq!(
        exported.profiles[1].contents["OPENAI_API_KEY"],
        "synthetic-key"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(!fs::read_dir(&paths.codex).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".export.json.tmp-")
    }));
    assert!(
        export_profiles(&paths, None, vec![], output, false)
            .unwrap_err()
            .contains("already exists")
    );
    assert!(
        export_profiles(
            &paths,
            None,
            vec!["absent".into()],
            paths.codex.join("absent.json"),
            false
        )
        .is_err()
    );
    export_profiles(
        &paths,
        Some("Work".into()),
        vec![],
        paths.codex.join("labeled.json"),
        false,
    )
    .unwrap();
}

#[test]
fn export_preserves_a_destination_created_after_the_initial_check() {
    use crate::common::AtomicCommitHookGuard;

    let (dir, paths) = setup();
    write_profile(&paths, "saved", "synthetic-account");
    let output = dir.path().join("exports/bundle.json");
    let _hook = AtomicCommitHookGuard::new(|temporary, destination| {
        assert!(!destination.exists());
        let staged: ExportBundle = serde_json::from_slice(&fs::read(temporary).unwrap()).unwrap();
        assert_eq!(staged.profiles.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(temporary).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(destination, b"synthetic competing output").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(destination, fs::Permissions::from_mode(0o640)).unwrap();
        }
    });

    let result = export_profiles(&paths, None, vec![], output.clone(), false);
    assert_eq!(fs::read(&output).unwrap(), b"synthetic competing output");
    assert_eq!(
        result.unwrap_err(),
        format!("Error: Export file already exists: {}", output.display())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    let remaining: Vec<_> = fs::read_dir(output.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(remaining, vec![output]);
}

#[cfg(unix)]
#[test]
fn export_preserves_a_dangling_destination_symlink() {
    use std::os::unix::fs::symlink;

    let (dir, paths) = setup();
    write_profile(&paths, "saved", "synthetic-account");
    let exports = dir.path().join("exports");
    fs::create_dir(&exports).unwrap();
    let output = exports.join("bundle.json");
    let target = dir.path().join("missing-target.json");
    symlink(&target, &output).unwrap();
    assert!(
        !output.exists(),
        "dangling link passes the initial existence check"
    );

    let error = export_profiles(&paths, None, vec![], output.clone(), false).unwrap_err();
    assert_eq!(
        error,
        format!("Error: Export file already exists: {}", output.display())
    );
    assert_eq!(fs::read_link(&output).unwrap(), target);
    assert!(!target.exists());
    assert_eq!(fs::read_dir(exports).unwrap().count(), 1);
}

#[test]
fn export_write_failures_remove_temporary_files_without_creating_the_destination() {
    use crate::common::{
        FAIL_WRITE_OPEN, FAIL_WRITE_PERMS, FAIL_WRITE_RENAME, FAIL_WRITE_SYNC, FAIL_WRITE_WRITE,
        FailpointGuard,
    };

    for step in [
        FAIL_WRITE_OPEN,
        FAIL_WRITE_WRITE,
        FAIL_WRITE_PERMS,
        FAIL_WRITE_SYNC,
        FAIL_WRITE_RENAME,
    ] {
        if step == FAIL_WRITE_PERMS && !cfg!(unix) {
            continue;
        }
        let (dir, paths) = setup();
        write_profile(&paths, "saved", "synthetic-account");
        let output = dir.path().join("exports/bundle.json");
        let _failure = FailpointGuard::new(step, 1);
        assert!(export_profiles(&paths, None, vec![], output.clone(), false).is_err());
        assert!(!output.exists());
        assert_eq!(fs::read_dir(output.parent().unwrap()).unwrap().count(), 0);
    }
}

#[test]
fn export_rejects_corrupt_or_unreadable_saved_profiles() {
    let (_dir, paths) = setup();
    fs::write(paths.profiles.join("corrupt.json"), "{").unwrap();
    assert!(
        export_profiles(
            &paths,
            None,
            vec!["corrupt".into()],
            paths.codex.join("out.json"),
            false
        )
        .unwrap_err()
        .contains("invalid JSON")
    );
    fs::create_dir(paths.profiles.join("unreadable.json")).unwrap();
    assert!(
        export_profiles(
            &paths,
            None,
            vec!["unreadable".into()],
            paths.codex.join("out.json"),
            false
        )
        .is_err()
    );
}

#[test]
fn metadata_repairs_normalize_and_preserve_each_invalid_backup() {
    let (_dir, paths) = setup();
    fs::write(
        &paths.profiles_index,
        r#"{"version":0,"profiles":{},"last_used":"old"}"#,
    )
    .unwrap();
    let repairs = repair_profiles_metadata(&paths).unwrap();
    assert!(repairs.iter().any(|item| item.contains("Normalized")));
    assert_eq!(
        read_profiles_index(&paths).unwrap().version,
        PROFILES_INDEX_VERSION
    );
    for suffix in ["json.bak", "json.bak.1", "json.bak.2"] {
        fs::write(&paths.profiles_index, "invalid").unwrap();
        repair_profiles_metadata(&paths).unwrap();
        assert_eq!(
            fs::read_to_string(paths.profiles_index.with_extension(suffix)).unwrap(),
            "invalid"
        );
    }
    fs::remove_file(&paths.profiles_index).unwrap();
    fs::create_dir(&paths.profiles_index).unwrap();
    assert!(repair_profiles_metadata(&paths).is_err());
    assert!(read_profiles_index(&paths).is_err());
    assert!(read_profiles_index_relaxed(&paths).profiles.is_empty());
}

#[test]
fn rename_collisions_keep_identity_and_metadata() {
    let (_dir, paths) = setup();
    let tokens = write_profile(&paths, "old", "wanted");
    let identity = extract_profile_identity(&tokens).unwrap();
    let mut index = ProfilesIndex::default();
    index.profiles.insert(
        "old".into(),
        ProfileIndexEntry {
            label: Some("Preserved".into()),
            ..Default::default()
        },
    );
    assert_eq!(
        rename_profile_id(&paths, &mut index, "old", "old", &identity).unwrap(),
        "old"
    );
    write_profile(&paths, "new", "collision");
    let suffix = short_identity_suffix(&identity);
    write_profile(&paths, &format!("new-{suffix}"), "collision");
    let id = rename_profile_id(&paths, &mut index, "old", "new", &identity).unwrap();
    assert_eq!(id, format!("new-{suffix}-2"));
    assert_eq!(index.profiles[&id].label.as_deref(), Some("Preserved"));
    assert!(!index.profiles.contains_key("old"));
    fs::create_dir(paths.profiles.join("blocked.json")).unwrap();
    assert!(
        rename_profile_id(&paths, &mut index, &id, "blocked", &identity)
            .unwrap_err()
            .contains("rename")
    );
    assert!(profile_path_for_id(&paths.profiles, &id).is_file());
}

#[test]
fn sync_merges_duplicate_identity_names_and_ignores_incomplete_auth() {
    let (_dir, paths) = setup();
    let tokens = write_profile(&paths, "alias-a", "same");
    write_profile(&paths, "alias-b", "same");
    let mut index = ProfilesIndex::default();
    let id = resolve_sync_id(&paths, &mut index, &tokens)
        .unwrap()
        .unwrap();
    assert_eq!(id, "alias-a");
    assert!(profile_path_for_id(&paths.profiles, "alias-a").is_file());
    assert!(profile_path_for_id(&paths.profiles, "alias-b").is_file());
    assert!(!profile_path_for_id(&paths.profiles, "storage@example.com-plus").exists());
    assert_eq!(
        resolve_sync_id(&paths, &mut index, &tokens).unwrap(),
        Some(id)
    );
    write_profile(&paths, "storage@example.com-plus", "same");
    assert_eq!(
        resolve_sync_id(&paths, &mut index, &tokens).unwrap(),
        Some("storage@example.com-plus".into())
    );
    let incomplete = Tokens {
        account_id: None,
        id_token: None,
        access_token: None,
        refresh_token: None,
    };
    assert!(
        resolve_sync_id(&paths, &mut index, &incomplete)
            .unwrap()
            .is_none()
    );
}

#[test]
fn revalidation_rejects_missing_selected_tokens_and_changed_auth() {
    let (_dir, paths) = setup();
    let tokens = write_profile(&paths, "selected", "same");
    fs::write(&paths.auth, auth("same").to_string()).unwrap();
    let raw = read_auth_contents_opt(&paths.auth);
    let snapshot = Snapshot {
        labels: Labels::new(),
        tokens: BTreeMap::new(),
        index: ProfilesIndex::default(),
    };
    assert!(
        revalidate_load_state(&paths, &snapshot, "selected", &Some(tokens.clone()), &raw).is_err()
    );
    let snapshot = Snapshot {
        tokens: BTreeMap::from([("selected".into(), Ok(tokens.clone()))]),
        ..snapshot
    };
    revalidate_load_state(&paths, &snapshot, "selected", &Some(tokens.clone()), &raw).unwrap();
    assert!(revalidate_load_state(&paths, &snapshot, "selected", &None, &raw).is_err());
    fs::write(&paths.auth, auth("other").to_string()).unwrap();
    assert!(
        revalidate_load_state(&paths, &snapshot, "selected", &Some(tokens.clone()), &raw).is_err()
    );
    fs::write(&paths.auth, raw.as_ref().unwrap()).unwrap();
    fs::remove_file(paths.profiles.join("selected.json")).unwrap();
    assert!(revalidate_load_state(&paths, &snapshot, "selected", &Some(tokens), &raw).is_err());
}

#[test]
fn sync_checks_both_account_identity_and_latest_saved_tokens() {
    let (_dir, paths) = setup();
    let expected = write_profile(&paths, "selected", "same");
    fs::write(&paths.auth, auth("same").to_string()).unwrap();
    let target = paths.profiles.join("selected.json");
    sync_profile_with_lock(&paths, &target, &expected, Some(&expected)).unwrap();
    let changed = write_profile(&paths, "selected", "other");
    assert!(sync_profile_with_lock(&paths, &target, &expected, Some(&expected)).is_err());
    assert!(sync_profile_with_lock(&paths, &target, &expected, None).is_err());
    assert_eq!(read_tokens(&target).unwrap(), changed);
    fs::remove_file(&target).unwrap();
    assert!(sync_profile_with_lock(&paths, &target, &expected, None).is_err());
    fs::remove_file(&paths.auth).unwrap();
    assert!(sync_profile_with_lock(&paths, &target, &expected, None).is_err());
    assert!(sync_profile(&paths, &target).is_err());
}

#[test]
fn helpers_handle_unusable_paths_and_empty_identity_components() {
    let (_dir, paths) = setup();
    assert!(paths_for_profile_source(Path::new("/")).is_err());
    assert!(paths_for_profile_source(Path::new("relative.json")).is_err());
    assert_eq!(profile_base("***", "???"), "unknown-unknown");
    assert_eq!(sanitize_part(" A ** B "), "a-b");
    assert_eq!(
        short_identity_suffix(&ProfileIdentityKey {
            principal_id: String::new(),
            workspace_or_org_id: "unknown".into(),
            plan_type: "plus".into()
        }),
        "id"
    );
    fs::remove_dir(&paths.profiles).unwrap();
    fs::write(&paths.profiles, "file").unwrap();
    assert!(profile_files(&paths.profiles).is_err());
}

#[test]
fn labels_and_readonly_commands_handle_missing_and_corrupt_metadata() {
    let (_dir, paths) = setup();
    list_profiles(&paths, true, false).unwrap();
    list_profiles(&paths, false, false).unwrap();
    status_selected_profile(&paths, None, Some("missing"), false).unwrap();
    status_selected_profile(&paths, None, Some("missing"), true).unwrap();
    loaded_profile_status(&paths).unwrap();
    delete_profile(&paths, true, None, vec!["missing".into()], false).unwrap();
    let tokens = write_profile(&paths, "saved", "saved");
    let mut index = ProfilesIndex::default();
    update_profiles_index_entry(&mut index, "saved", Some(&tokens), Some("Work".into()));
    write_profiles_index(&paths, &index).unwrap();
    let store = ProfileStore::load(&paths).unwrap();
    assert_eq!(
        resolve_label_target_id(&store, Some("Work"), None).unwrap(),
        "saved"
    );
    assert!(resolve_label_target_id(&store, None, Some("absent")).is_err());
    assert!(resolve_label_target_id(&store, None, None).is_err());
    drop(store);
    fs::write(&paths.profiles_index, "{").unwrap();
    assert!(delete_profile(&paths, true, None, vec!["saved".into()], false).is_err());
    assert!(ProfileStore::load(&paths).is_err());
    assert_eq!(load_snapshot(&paths, false).unwrap().tokens.len(), 1);
}

#[test]
fn directory_iteration_failure_never_returns_a_partial_profile_list() {
    let entries = [
        Ok(PathBuf::from("valid.json")),
        Err(io::Error::other("directory disconnected")),
    ];
    let error = collect_profile_files(entries.into_iter()).unwrap_err();
    assert!(error.contains("directory disconnected"));
}

#[test]
fn metadata_repairs_prune_stale_entries_and_ignore_bad_labels() {
    let (_dir, paths) = setup();
    write_profile(&paths, "existing", "account");
    let mut index = ProfilesIndex::default();
    for (id, label) in [
        ("existing", " Work "),
        ("stale", "Work"),
        ("also-stale", "  "),
    ] {
        index.profiles.insert(
            id.into(),
            ProfileIndexEntry {
                label: Some(label.into()),
                ..Default::default()
            },
        );
    }
    assert_eq!(
        labels_from_index(&index),
        Labels::from([("Work".into(), "existing".into())])
    );
    write_profiles_index(&paths, &index).unwrap();
    let repairs = repair_profiles_metadata(&paths).unwrap();
    assert!(
        repairs
            .iter()
            .any(|item| item == "Pruned 2 stale profile index entries")
    );
    assert_eq!(read_profiles_index(&paths).unwrap().profiles.len(), 1);
    index.profiles.remove("also-stale");
    write_profiles_index(&paths, &index).unwrap();
    assert!(
        repair_profiles_metadata(&paths)
            .unwrap()
            .iter()
            .any(|item| item == "Pruned 1 stale profile index entry")
    );
    let incomplete = Tokens {
        account_id: None,
        id_token: None,
        access_token: None,
        refresh_token: None,
    };
    update_profiles_index_entry(&mut index, "incomplete", Some(&incomplete), None);
    assert!(index.profiles["incomplete"].principal_id.is_none());
}

#[test]
fn selection_rejects_stale_labels_and_deduplicates_explicit_ids() {
    let labels = Labels::from([("missing".into(), "missing-id".into())]);
    let candidates = vec![Candidate {
        id: "present".into(),
        display: "Present".into(),
    }];
    assert!(select_by_label("missing", &labels, &candidates).is_err());
    let selected = select_many_by_id(&["present".into(), "present".into()], &candidates).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].id, "present");
}

#[test]
fn invalid_profiles_cannot_be_loaded_and_bad_status_config_is_reported() {
    let (_dir, paths) = setup();
    fs::write(paths.profiles.join("broken.json"), "{}").unwrap();
    assert!(
        load_profile(&paths, None, Some("broken".into()), true, false, false)
            .unwrap_err()
            .contains("invalid")
    );
    fs::write(
        &paths.auth,
        serde_json::json!({"OPENAI_API_KEY":"test"}).to_string(),
    )
    .unwrap();
    save_profile(&paths, Some("api".into()), false).unwrap();
    let tokens = read_tokens(&paths.auth).unwrap();
    let id = current_saved_id(&paths, &load_profile_tokens_map(&paths).unwrap()).unwrap();
    assert!(is_api_key_profile(&tokens));
    fs::write(paths.codex.join("config.toml"), "not valid toml {").unwrap();
    status_all_profiles(&paths, false, false).unwrap();
    assert!(load_profile(&paths, None, Some(id.clone()), true, true, true).is_err());
    fs::write(
        paths.codex.join("config.toml"),
        "chatgpt_base_url = 'ftp://localhost'",
    )
    .unwrap();
    load_profile(&paths, None, Some(id.clone()), true, true, true).unwrap();
    load_profile(&paths, None, Some(id), true, true, false).unwrap();
}

#[test]
fn incomplete_identity_with_whitespace_principal_is_rejected() {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let (_dir, paths) = setup();
    let mut tokens = write_profile(&paths, "saved", "account");
    let payload = serde_json::json!({"sub":" ","email":"storage@example.com","https://api.openai.com/auth":{"chatgpt_plan_type":"plus"}});
    tokens.id_token = Some(format!(
        "e30.{}.",
        URL_SAFE_NO_PAD.encode(payload.to_string())
    ));
    assert!(resolve_save_id(&paths, &mut ProfilesIndex::default(), &tokens).is_err());
    assert!(
        resolve_sync_id(&paths, &mut ProfilesIndex::default(), &tokens)
            .unwrap()
            .is_none()
    );
}

// APFS rejects invalid UTF-8 before profile discovery can inspect the entry.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_profile_names_are_ignored() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let (_dir, paths) = setup();
    let name = OsString::from_vec(b"\xff.json".to_vec());
    fs::write(paths.profiles.join(name), auth("account").to_string()).unwrap();
    assert!(load_profile_tokens_map(&paths).unwrap().is_empty());
    assert!(
        scan_profile_ids(
            &paths.profiles,
            &extract_profile_identity(&read_tokens_from_value()).unwrap()
        )
        .unwrap()
        .is_empty()
    );
}

#[cfg(target_os = "linux")]
fn read_tokens_from_value() -> Tokens {
    serde_json::from_value::<AuthFile>(auth("account"))
        .unwrap()
        .tokens
        .unwrap()
}

#[test]
fn prompt_result_errors_remain_actionable() {
    let (_dir, paths) = setup();
    assert!(
        prompt_unsaved_load_with(
            &paths,
            "unsaved",
            true,
            Err(inquire::error::InquireError::NotTTY)
        )
        .unwrap_err()
        .contains("prompt")
    );
    assert!(
        confirm_delete_profiles_with(true, Err(inquire::error::InquireError::NotTTY))
            .unwrap_err()
            .contains("prompt")
    );
}

#[test]
fn copying_a_missing_profile_reports_action_and_destination() {
    let (_dir, paths) = setup();
    let error = copy_profile(
        &paths.profiles.join("missing.json"),
        &paths.auth,
        PROFILE_COPY_CONTEXT_LOAD,
    )
    .unwrap_err();
    assert!(error.contains(PROFILE_COPY_CONTEXT_LOAD));
    assert!(error.contains(paths.auth.to_str().unwrap()));
    assert!(!paths.auth.exists());
}

#[test]
fn load_validation_rejects_missing_snapshot_and_missing_file() {
    let (_dir, paths) = setup();
    let snapshot = Snapshot {
        labels: Labels::new(),
        tokens: BTreeMap::new(),
        index: ProfilesIndex::default(),
    };
    assert!(
        validate_selected_profile(&snapshot, "missing", false)
            .unwrap_err()
            .contains("not found")
    );
    assert!(
        copy_selected_profile(&paths, "missing", false)
            .unwrap_err()
            .contains("not found")
    );
    assert!(!paths.auth.exists());
}

#[test]
fn load_json_preserves_success_when_optional_status_fails() {
    let error = loaded_profile_json(
        "saved",
        Some("Work".into()),
        Some(Err("Error: status failed".into())),
    );
    assert_eq!(error["id"], "saved");
    assert_eq!(error["label"], "Work");
    assert_eq!(error["status_error"], "status failed");
    assert!(error.get("status").is_none());
    let without_status = loaded_profile_json("saved", None, None);
    assert_eq!(
        without_status,
        serde_json::json!({"id":"saved","label":null})
    );
    let status = serde_json::json!({"usage":{"status":"ok"}});
    let success = loaded_profile_json("saved", None, Some(Ok(status.clone())));
    assert_eq!(success["status"], status);
    assert!(success.get("status_error").is_none());
}

#[test]
fn load_revalidation_rejects_changed_selected_tokens_and_store_mode() {
    let (_dir, paths) = setup();
    let tokens = write_profile(&paths, "selected", "same");
    fs::write(&paths.auth, auth("same").to_string()).unwrap();
    let raw = read_auth_contents_opt(&paths.auth);
    let snapshot = Snapshot {
        labels: Labels::new(),
        tokens: BTreeMap::from([("selected".into(), Ok(tokens.clone()))]),
        index: ProfilesIndex::default(),
    };
    write_profile(&paths, "selected", "changed");
    assert_eq!(
        revalidate_load_state(&paths, &snapshot, "selected", &Some(tokens.clone()), &raw)
            .unwrap_err(),
        AUTH_ERR_REFRESH_STATE_CHANGED
    );
    fs::write(
        paths.codex.join("config.toml"),
        "cli_auth_credentials_store = 'keyring'",
    )
    .unwrap();
    assert_eq!(
        revalidate_load_state(&paths, &snapshot, "selected", &Some(tokens), &raw).unwrap_err(),
        AUTH_ERR_REFRESH_STATE_CHANGED
    );
}

#[test]
fn human_import_and_unfiltered_export_round_trip_an_account() {
    let (_dir, paths) = setup();
    let input = bundle(
        &paths,
        serde_json::json!([{"id":"one","contents":auth("one")}]),
    );
    import_profiles(&paths, input, false).unwrap();
    let output = paths.codex.join("all.json");
    export_profiles(&paths, None, Vec::new(), output.clone(), false).unwrap();
    let exported: ExportBundle = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(exported.profiles.len(), 1);
    assert_eq!(exported.profiles[0].id, "one");
}

#[test]
fn metadata_repair_initializes_and_indexes_only_ready_profiles() {
    let (_dir, paths) = setup();
    write_profile(&paths, "one", "one");
    write_profile(&paths, "two", "two");
    fs::write(paths.profiles.join("invalid.json"), "{}").unwrap();
    let repairs = repair_profiles_metadata(&paths).unwrap();
    assert!(
        repairs
            .iter()
            .any(|repair| repair == "Initialized profiles index")
    );
    assert!(
        repairs
            .iter()
            .any(|repair| repair == "Indexed 2 saved profiles")
    );
    assert_eq!(read_profiles_index(&paths).unwrap().profiles.len(), 2);
    assert!(repair_profiles_metadata(&paths).unwrap().is_empty());
    write_profile(&paths, "three", "three");
    assert!(
        repair_profiles_metadata(&paths)
            .unwrap()
            .iter()
            .any(|repair| repair == "Indexed 1 saved profile")
    );
}

#[test]
fn metadata_labels_can_be_updated_without_cached_tokens() {
    let mut index = ProfilesIndex::default();
    update_profiles_index_entry(&mut index, "id", None, Some("New label".into()));
    assert_eq!(index.profiles["id"].label.as_deref(), Some("New label"));
    assert!(index.profiles["id"].account_id.is_none());
}

#[test]
fn synchronization_handles_absent_and_changed_active_credentials() {
    let (_dir, paths) = setup();
    sync_current(&paths, &mut ProfilesIndex::default()).unwrap();
    let expected = write_profile(&paths, "selected", "same");
    fs::write(&paths.auth, auth("changed").to_string()).unwrap();
    let before = fs::read(paths.profiles.join("selected.json")).unwrap();
    assert_eq!(
        sync_profile_with_lock(
            &paths,
            &paths.profiles.join("selected.json"),
            &expected,
            Some(&expected)
        )
        .unwrap_err(),
        AUTH_ERR_REFRESH_STATE_CHANGED
    );
    assert_eq!(
        fs::read(paths.profiles.join("selected.json")).unwrap(),
        before
    );
}

#[test]
fn inquire_result_preserves_values_cancellation_and_terminal_errors() {
    assert_eq!(handle_inquire_result(Ok(7), "selection").unwrap(), 7);
    assert_eq!(
        handle_inquire_result::<i32>(
            Err(inquire::error::InquireError::OperationCanceled),
            "selection"
        )
        .unwrap_err(),
        CANCELLED_MESSAGE
    );
    assert!(
        handle_inquire_result::<i32>(Err(inquire::error::InquireError::NotTTY), "selection")
            .unwrap_err()
            .contains("selection")
    );
}

#[cfg(unix)]
#[test]
fn import_target_inspection_errors_preserve_all_existing_files() {
    let (_dir, paths) = setup();
    write_profile(&paths, "existing", "keep");
    let before = fs::read(paths.profiles.join("existing.json")).unwrap();
    let input = bundle(
        &paths,
        serde_json::json!([
            {"id":"first","contents":auth("new")},
            {"id":"x".repeat(512),"contents":auth("too-long")}
        ]),
    );
    let error = import_profiles(&paths, input, false).unwrap_err();
    assert!(error.contains("Could not inspect profile"));
    assert!(!paths.profiles.join("first.json").exists());
    assert_eq!(
        fs::read(paths.profiles.join("existing.json")).unwrap(),
        before
    );
}
