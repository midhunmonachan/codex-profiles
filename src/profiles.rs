use chrono::{DateTime, Local};
use colored::Colorize;
use inquire::{Confirm, MultiSelect, Select};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal as _};
use std::path::Component;
use std::path::{Path, PathBuf};

use crate::json_response::CommandResultJson;
use crate::{
    AUTH_ERR_INCOMPLETE_ACCOUNT, AUTH_ERR_PROFILE_MISSING_EMAIL_PLAN, PROFILE_COPY_CONTEXT_LOAD,
    PROFILE_COPY_CONTEXT_SAVE, PROFILE_DELETE_HELP, PROFILE_ERR_COPY_CONTEXT,
    PROFILE_ERR_CURRENT_NOT_SAVED, PROFILE_ERR_DELETE_CONFIRM_REQUIRED, PROFILE_ERR_FAILED_DELETE,
    PROFILE_ERR_ID_NO_MATCH, PROFILE_ERR_ID_NOT_FOUND, PROFILE_ERR_INDEX_INVALID_JSON,
    PROFILE_ERR_LABEL_EMPTY, PROFILE_ERR_LABEL_EXISTS, PROFILE_ERR_LABEL_NO_MATCH,
    PROFILE_ERR_LABEL_NOT_FOUND, PROFILE_ERR_PROMPT_CONTEXT, PROFILE_ERR_PROMPT_DELETE,
    PROFILE_ERR_PROMPT_LOAD, PROFILE_ERR_READ_INDEX, PROFILE_ERR_READ_PROFILES_DIR,
    PROFILE_ERR_RENAME_PROFILE, PROFILE_ERR_SELECTED_INVALID, PROFILE_ERR_SERIALIZE_INDEX,
    PROFILE_ERR_SYNC_CURRENT, PROFILE_ERR_TTY_REQUIRED, PROFILE_ERR_WRITE_INDEX, PROFILE_LOAD_HELP,
    PROFILE_MSG_DELETED_COUNT, PROFILE_MSG_DELETED_WITH, PROFILE_MSG_LABEL_CLEARED,
    PROFILE_MSG_LABEL_SET, PROFILE_MSG_LOADED_WITH, PROFILE_MSG_NOT_FOUND, PROFILE_MSG_SAVED,
    PROFILE_MSG_SAVED_WITH, PROFILE_PROMPT_CANCEL, PROFILE_PROMPT_CONTINUE_WITHOUT_SAVING,
    PROFILE_PROMPT_DELETE_MANY, PROFILE_PROMPT_DELETE_ONE, PROFILE_PROMPT_DELETE_SELECTED,
    PROFILE_PROMPT_SAVE_AND_CONTINUE, PROFILE_SUMMARY_AUTH_ERROR, PROFILE_SUMMARY_ERROR,
    PROFILE_SUMMARY_FILE_MISSING, PROFILE_SUMMARY_USAGE_ERROR, PROFILE_UNSAVED_NO_MATCH,
    PROFILE_WARN_CURRENT_NOT_SAVED_REASON, UI_ERROR_PREFIX, UI_ERROR_TWO_LINE,
};
use crate::{
    AuthFile, ProfileIdentityKey, Tokens, extract_email_and_plan, extract_profile_identity,
    is_api_key_profile, is_free_plan, is_profile_ready, profile_error, read_tokens,
    read_tokens_opt, require_identity, token_account_id, tokens_from_api_key,
};
use crate::{
    CANCELLED_MESSAGE, format_action, format_entry_header, format_error, format_label_later_hint,
    format_list_hint, format_no_profiles, format_save_before_load_or_force, format_unsaved_warning,
    format_warning, inquire_select_render_config, is_inquire_cancel, is_plain, normalize_error,
    print_output_block, style_text, use_color_stderr, use_color_stdout,
};
use crate::{
    Paths, USAGE_UNAVAILABLE_API_KEY_DETAIL, USAGE_UNAVAILABLE_API_KEY_TITLE, command_name,
    copy_atomic, write_atomic,
};
use crate::{UsageLock, format_usage_unavailable, lock_usage, read_base_url, usage_unavailable};

const DEFAULT_USAGE_CONCURRENCY: usize = 32;
const MAX_USAGE_CONCURRENCY: usize = 128;
const USAGE_CONCURRENCY_ENV: &str = "CODEX_PROFILES_USAGE_CONCURRENCY";

#[derive(Serialize, Deserialize)]
struct ExportBundle {
    version: u8,
    profiles: Vec<ExportedProfile>,
}

#[derive(Serialize, Deserialize)]
struct ExportedProfile {
    id: String,
    label: Option<String>,
    contents: serde_json::Value,
}

struct PreparedImportProfile {
    id: String,
    label: Option<String>,
    contents: Vec<u8>,
    tokens: Tokens,
}

pub fn save_profile(paths: &Paths, label: Option<String>, json: bool) -> Result<(), String> {
    let use_color = use_color_stdout();
    let mut store = ProfileStore::load(paths)?;
    let tokens = read_tokens(&paths.auth)?;
    let id = resolve_save_id(paths, &mut store.profiles_index, &tokens)?;

    if let Some(label) = label.as_deref() {
        assign_label(&mut store.labels, label, &id)?;
    }

    let target = profile_path_for_id(&paths.profiles, &id);
    copy_profile(&paths.auth, &target, PROFILE_COPY_CONTEXT_SAVE)?;

    let label_display = label_for_id(&store.labels, &id);
    update_profiles_index_entry(
        &mut store.profiles_index,
        &id,
        Some(&tokens),
        label_display.clone(),
    );
    store.save(paths)?;

    if json {
        let result = CommandResultJson::success(
            "save",
            serde_json::json!({
                "id": id,
                "label": label_display,
            }),
        );
        result.print()?;
        return Ok(());
    }

    let info = profile_info(Some(&tokens), label_display.clone(), true, use_color);
    let message = if info.email.is_some() {
        crate::msg1(PROFILE_MSG_SAVED_WITH, info.display)
    } else {
        PROFILE_MSG_SAVED.to_string()
    };
    let mut message = format_action(&message, use_color);
    if label_display.is_none() {
        message.push('\n');
        message.push_str(&format_label_later_hint(&id, use_color));
    }
    print_output_block(&message);
    Ok(())
}

pub fn set_profile_label(
    paths: &Paths,
    label: Option<String>,
    id: Option<String>,
    to: String,
    json: bool,
) -> Result<(), String> {
    let use_color = use_color_stdout();
    let mut store = ProfileStore::load(paths)?;
    let target_id = resolve_label_target_id(&store, label.as_deref(), id.as_deref())?;
    let target_label = trim_label(&to)?.to_string();

    assign_label(&mut store.labels, &target_label, &target_id)?;
    store.save(paths)?;

    if json {
        let result = CommandResultJson::success(
            "label set",
            serde_json::json!({
                "id": target_id,
                "label": target_label,
            }),
        );
        result.print()?;
        return Ok(());
    }

    let message = format_action(
        &crate::msg2(PROFILE_MSG_LABEL_SET, target_label, target_id),
        use_color,
    );
    print_output_block(&message);
    Ok(())
}

pub fn clear_profile_label(
    paths: &Paths,
    label: Option<String>,
    id: Option<String>,
    json: bool,
) -> Result<(), String> {
    let use_color = use_color_stdout();
    let mut store = ProfileStore::load(paths)?;
    let target_id = resolve_label_target_id(&store, label.as_deref(), id.as_deref())?;

    remove_labels_for_id(&mut store.labels, &target_id);
    store.save(paths)?;

    if json {
        let result = CommandResultJson::success(
            "label clear",
            serde_json::json!({
                "id": target_id,
                "label": null,
            }),
        );
        result.print()?;
        return Ok(());
    }

    let message = format_action(
        &crate::msg1(PROFILE_MSG_LABEL_CLEARED, target_id),
        use_color,
    );
    print_output_block(&message);
    Ok(())
}

pub fn rename_profile_label(
    paths: &Paths,
    label: String,
    to: String,
    json: bool,
) -> Result<(), String> {
    let use_color = use_color_stdout();
    let mut store = ProfileStore::load(paths)?;
    let old_label = trim_label(&label)?.to_string();
    let target_id = resolve_label_id(&store.labels, &old_label)?;
    let new_label = trim_label(&to)?.to_string();

    assign_label(&mut store.labels, &new_label, &target_id)?;
    store.save(paths)?;

    if json {
        let result = CommandResultJson::success(
            "label rename",
            serde_json::json!({
                "id": target_id,
                "label": new_label,
            }),
        );
        result.print()?;
        return Ok(());
    }

    let message = format_action(
        &format!("Renamed label '{}' to '{}'", old_label, new_label),
        use_color,
    );
    print_output_block(&message);
    Ok(())
}

pub fn export_profiles(
    paths: &Paths,
    label: Option<String>,
    ids: Vec<String>,
    output: PathBuf,
    json: bool,
) -> Result<(), String> {
    if output.exists() {
        return Err(format!(
            "Error: Export file already exists: {}",
            output.display()
        ));
    }

    let use_color = use_color_stdout();
    let store = ProfileStore::load(paths)?;
    let selected_ids = resolve_export_ids(paths, &store, label.as_deref(), &ids)?;
    let mut profiles = Vec::with_capacity(selected_ids.len());

    for id in selected_ids {
        let path = profile_path_for_id(&paths.profiles, &id);
        let raw = fs::read_to_string(&path)
            .map_err(|err| crate::msg2(PROFILE_ERR_READ_PROFILES_DIR, path.display(), err))?;
        let contents: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|err| format!("Error: Saved profile '{}' is invalid JSON: {err}", id))?;
        profiles.push(ExportedProfile {
            label: label_for_id(&store.labels, &id),
            id,
            contents,
        });
    }

    let bundle = ExportBundle {
        version: 1,
        profiles,
    };
    let mut bytes = serde_json::to_vec_pretty(&bundle).map_err(|err| err.to_string())?;
    bytes.push(b'\n');
    crate::common::write_atomic_private(&output, &bytes)?;
    tighten_export_permissions(&output)?;

    let count = bundle.profiles.len();
    let noun = if count == 1 { "profile" } else { "profiles" };

    if json {
        let result = CommandResultJson::success(
            "export",
            serde_json::json!({
                "path": output.display().to_string(),
                "count": count,
            }),
        );
        result.print()?;
        return Ok(());
    }

    let message = format_action(
        &format!("Exported {count} {noun} to {}", output.display()),
        use_color,
    );
    print_output_block(&message);
    Ok(())
}

pub fn import_profiles(paths: &Paths, input: PathBuf, json: bool) -> Result<(), String> {
    let use_color = use_color_stdout();
    let raw = fs::read_to_string(&input).map_err(|err| {
        format!(
            "Error: Could not read import file {}: {err}",
            input.display()
        )
    })?;
    let bundle: ExportBundle = serde_json::from_str(&raw)
        .map_err(|err| format!("Error: Import file is invalid JSON: {err}"))?;
    if bundle.version != 1 {
        return Err(format!(
            "Error: Import file version {} is not supported.",
            bundle.version
        ));
    }

    let mut store = ProfileStore::load(paths)?;
    let existing_ids = collect_profile_ids(&paths.profiles)?;
    let mut staged_labels = store.labels.clone();
    let mut seen_ids = HashSet::new();
    let mut prepared = Vec::with_capacity(bundle.profiles.len());
    for profile in bundle.profiles {
        validate_import_profile_id(&profile.id)?;
        if !seen_ids.insert(profile.id.clone()) {
            return Err(format!(
                "Error: Import bundle contains duplicate profile id '{}'.",
                profile.id
            ));
        }
        if existing_ids.contains(&profile.id) {
            return Err(format!("Error: Profile '{}' already exists.", profile.id));
        }
        if let Some(label) = profile.label.as_deref() {
            assign_label(&mut staged_labels, label, &profile.id)?;
        }
        prepared.push(prepare_import_profile(profile)?);
    }

    let mut written_ids = Vec::with_capacity(prepared.len());
    for profile in &prepared {
        let path = profile_path_for_id(&paths.profiles, &profile.id);
        if let Err(err) = crate::common::write_atomic_private(&path, &profile.contents) {
            cleanup_imported_profiles(paths, &written_ids);
            return Err(err);
        }
        written_ids.push(profile.id.clone());
    }

    for profile in &prepared {
        if let Some(label) = profile.label.as_deref() {
            assign_label(&mut store.labels, label, &profile.id)?;
        }
        update_profiles_index_entry(
            &mut store.profiles_index,
            &profile.id,
            Some(&profile.tokens),
            profile.label.clone(),
        );
    }
    if let Err(err) = store.save(paths) {
        cleanup_imported_profiles(paths, &written_ids);
        return Err(err);
    }

    let count = prepared.len();
    let noun = if count == 1 { "profile" } else { "profiles" };

    if json {
        let imported: Vec<serde_json::Value> = prepared
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "label": p.label,
                })
            })
            .collect();
        let result = CommandResultJson::success(
            "import",
            serde_json::json!({
                "count": count,
                "profiles": imported,
            }),
        );
        result.print()?;
        return Ok(());
    }

    let message = format_action(
        &format!("Imported {count} {noun} from {}", input.display()),
        use_color,
    );
    print_output_block(&message);
    Ok(())
}

pub fn load_profile(
    paths: &Paths,
    label: Option<String>,
    id: Option<String>,
    force: bool,
    with_status: bool,
    json: bool,
) -> Result<(), String> {
    let use_color_err = use_color_stderr();
    let use_color_out = use_color_stdout();
    let no_profiles = format_no_profiles(paths, use_color_err);
    let (mut snapshot, mut ordered) = load_snapshot_ordered(paths, true, &no_profiles)?;

    if let Some(reason) = unsaved_reason(paths, &snapshot.tokens)
        && !force
    {
        match prompt_unsaved_load(paths, &reason)? {
            LoadChoice::SaveAndContinue => {
                save_profile(paths, None, false)?;
                let no_profiles = format_no_profiles(paths, use_color_err);
                let result = load_snapshot_ordered(paths, true, &no_profiles)?;
                snapshot = result.0;
                ordered = result.1;
            }
            LoadChoice::ContinueWithoutSaving => {}
            LoadChoice::Cancel => {
                return Err(CANCELLED_MESSAGE.to_string());
            }
        }
    }

    let candidates = make_candidates(paths, &snapshot, &ordered);
    let selected = pick_one(
        "load",
        label.as_deref(),
        id.as_deref(),
        &snapshot,
        &candidates,
    )?;
    let selected_id = selected.id.clone();
    let selected_display = selected.display.clone();

    match snapshot.tokens.get(&selected_id) {
        Some(Ok(_)) => {}
        Some(Err(err)) => {
            let message = err
                .strip_prefix(&format!("{} ", UI_ERROR_PREFIX))
                .unwrap_or(err);
            return Err(crate::msg1(PROFILE_ERR_SELECTED_INVALID, message));
        }
        None => {
            return Err(profile_not_found(use_color_err));
        }
    }

    let mut store = ProfileStore::load(paths)?;

    if let Err(err) = sync_current(paths, &mut store.profiles_index) {
        let warning = format_warning(&err, use_color_err);
        eprintln!("{warning}");
    }

    let source = profile_path_for_id(&paths.profiles, &selected_id);
    if !source.is_file() {
        return Err(profile_not_found(use_color_err));
    }

    copy_profile(&source, &paths.auth, PROFILE_COPY_CONTEXT_LOAD)?;

    let label = label_for_id(&store.labels, &selected_id);
    let tokens = snapshot
        .tokens
        .get(&selected_id)
        .and_then(|result| result.as_ref().ok());
    update_profiles_index_entry(
        &mut store.profiles_index,
        &selected_id,
        tokens,
        label.clone(),
    );
    store.save(paths)?;
    drop(store);

    let mut profile_json = serde_json::json!({
        "id": selected_id,
        "label": label,
    });
    if json && with_status {
        match current_status_json_value(paths) {
            Ok(status) => {
                profile_json["status"] = status;
            }
            Err(err) => {
                profile_json["status_error"] = serde_json::Value::String(normalize_error(&err));
            }
        }
    }

    if json {
        let result = CommandResultJson::success("load", profile_json);
        result.print()?;
        return Ok(());
    }

    let message = format_action(
        &crate::msg1(PROFILE_MSG_LOADED_WITH, selected_display),
        use_color_out,
    );
    print_output_block(&message);
    if with_status && let Err(err) = loaded_profile_status(paths) {
        let message = format!(
            "Loaded profile, but status retrieval failed: {}",
            normalize_error(&err)
        );
        eprintln!("{}", format_warning(&message, use_color_err));
    }
    Ok(())
}

pub fn delete_profile(
    paths: &Paths,
    yes: bool,
    label: Option<String>,
    ids: Vec<String>,
    json: bool,
) -> Result<(), String> {
    let use_color_out = use_color_stdout();
    let use_color_err = use_color_stderr();
    let no_profiles = format_no_profiles(paths, use_color_out);
    let (snapshot, ordered) = match load_snapshot_ordered(paths, true, &no_profiles) {
        Ok(result) => result,
        Err(message) => {
            if message == no_profiles {
                print_output_block(&message);
                return Ok(());
            }
            return Err(message);
        }
    };

    let candidates = make_candidates(paths, &snapshot, &ordered);
    let selections = pick_many("delete", label.as_deref(), &ids, &snapshot, &candidates)?;
    let (selected_ids, displays): (Vec<String>, Vec<String>) = selections
        .iter()
        .map(|item| (item.id.clone(), item.display.clone()))
        .unzip();

    if selected_ids.is_empty() {
        return Ok(());
    }

    let mut store = ProfileStore::load(paths)?;
    if !yes && !confirm_delete_profiles(&displays)? {
        return Err(CANCELLED_MESSAGE.to_string());
    }

    for selected in &selected_ids {
        let target = profile_path_for_id(&paths.profiles, selected);
        if !target.is_file() {
            return Err(profile_not_found(use_color_err));
        }
        fs::remove_file(&target).map_err(|err| crate::msg1(PROFILE_ERR_FAILED_DELETE, err))?;
        remove_labels_for_id(&mut store.labels, selected);
        store.profiles_index.profiles.remove(selected);
    }
    store.save(paths)?;

    if json {
        let deleted: Vec<serde_json::Value> = selected_ids
            .iter()
            .zip(displays.iter())
            .map(|(id, display)| serde_json::json!({ "id": id, "display": display }))
            .collect();
        let result = CommandResultJson::success(
            "delete",
            serde_json::json!({
                "count": selected_ids.len(),
                "deleted": deleted,
            }),
        );
        result.print()?;
        return Ok(());
    }

    let message = if selected_ids.len() == 1 {
        crate::msg1(PROFILE_MSG_DELETED_WITH, &displays[0])
    } else {
        crate::msg1(PROFILE_MSG_DELETED_COUNT, selected_ids.len())
    };
    let message = format_action(&message, use_color_out);
    print_output_block(&message);
    Ok(())
}

pub fn list_profiles(paths: &Paths, json: bool, show_id: bool) -> Result<(), String> {
    let snapshot = load_snapshot(paths, false)?;
    let current_saved_id = current_saved_id(paths, &snapshot.tokens);
    let ctx = ListCtx::new(paths, false, true, show_id);

    let ordered = ordered_profile_ids(&snapshot, current_saved_id.as_deref());
    let current_entry = make_current(
        paths,
        current_saved_id.as_deref(),
        &snapshot.labels,
        &snapshot.tokens,
        &ctx,
    );
    let has_saved = !ordered.is_empty();
    if !has_saved {
        if json {
            if let Some(entry) = current_entry {
                return print_list_json(&[entry]);
            }
            return print_list_json(&[]);
        }
        if let Some(entry) = current_entry {
            let lines = render_entries(&[entry], &ctx, false);
            print_output_block(&lines.join("\n"));
        } else {
            let message = format_no_profiles(paths, ctx.use_color);
            print_output_block(&message);
        }
        return Ok(());
    }

    let filtered: Vec<String> = ordered
        .into_iter()
        .filter(|id| current_saved_id.as_deref() != Some(id.as_str()))
        .collect();
    let list_entries = make_entries(&filtered, &snapshot, None, &ctx);

    if json {
        let mut entries = Vec::new();
        if let Some(entry) = current_entry {
            entries.push(entry);
        }
        entries.extend(list_entries);
        return print_list_json(&entries);
    }

    let mut lines = Vec::new();
    if let Some(entry) = current_entry.as_ref() {
        lines.extend(render_entries(std::slice::from_ref(entry), &ctx, false));
        if !list_entries.is_empty() {
            push_separator(&mut lines, false);
        }
    }
    lines.extend(render_entries(&list_entries, &ctx, false));
    let output = lines.join("\n");
    print_output_block(&output);
    Ok(())
}

pub fn status_profiles(
    paths: &Paths,
    all: bool,
    label: Option<String>,
    id: Option<String>,
    json: bool,
) -> Result<(), String> {
    if all {
        return status_all_profiles(paths, json);
    }

    if label.is_some() || id.is_some() {
        return status_selected_profile(paths, label.as_deref(), id.as_deref(), json);
    }

    status_current_profile(paths, json)
}

fn status_current_profile(paths: &Paths, json: bool) -> Result<(), String> {
    let (current_entry, ctx) = current_status_entry(paths, json)?;
    if json {
        return print_current_status_json(current_entry);
    }
    if let Some(entry) = current_entry {
        let lines = render_entries(std::slice::from_ref(&entry), &ctx, false);
        print_output_block(&lines.join("\n"));
    } else {
        let message = format_no_profiles(paths, ctx.use_color);
        print_output_block(&message);
    }
    Ok(())
}

fn current_status_entry(paths: &Paths, json: bool) -> Result<(Option<Entry>, ListCtx), String> {
    let snapshot = load_snapshot(paths, false)?;
    let current_saved_id = current_saved_id(paths, &snapshot.tokens);
    let mut ctx = ListCtx::new(paths, true, false, false);
    if json {
        ctx.use_color = false;
    }
    let labels = &snapshot.labels;
    let tokens_map = &snapshot.tokens;
    let current = make_current(paths, current_saved_id.as_deref(), labels, tokens_map, &ctx);
    Ok((current, ctx))
}

fn current_status_json_value(paths: &Paths) -> Result<serde_json::Value, String> {
    let (current, _) = current_status_entry(paths, true)?;
    let payload = current.map(status_profile_json);
    serde_json::to_value(payload).map_err(|err| crate::msg1(PROFILE_ERR_SERIALIZE_INDEX, err))
}

fn loaded_profile_status(paths: &Paths) -> Result<(), String> {
    let (current_entry, ctx) = current_status_entry(paths, false)?;
    let Some(entry) = current_entry else {
        let message = format_no_profiles(paths, ctx.use_color);
        print_output_block(&message);
        return Ok(());
    };
    if let Some(err) = entry.error_summary.as_deref() {
        return Err(err.to_string());
    }
    let lines = render_entry_details(&entry);
    print_output_block(&lines.join("\n"));
    Ok(())
}

fn status_selected_profile(
    paths: &Paths,
    label: Option<&str>,
    id: Option<&str>,
    json: bool,
) -> Result<(), String> {
    let use_color = use_color_stdout();
    let no_profiles = format_no_profiles(paths, use_color);
    let (snapshot, ordered) = match load_snapshot_ordered(paths, false, &no_profiles) {
        Ok(result) => result,
        Err(message) => {
            if message == no_profiles {
                if json {
                    return print_current_status_json(None);
                }
                print_output_block(&message);
                return Ok(());
            }
            return Err(message);
        }
    };
    let current_saved_id = current_saved_id(paths, &snapshot.tokens);
    let mut ctx = ListCtx::new(paths, true, false, false);
    if json {
        ctx.use_color = false;
    }

    let candidates = build_candidates(&ordered, &snapshot, current_saved_id.as_deref());
    let selected = if let Some(label) = label {
        select_by_label(label, &snapshot.labels, &candidates)?
    } else if let Some(id) = id {
        select_by_id(id, &candidates)?
    } else {
        unreachable!("status selector requires label or id")
    };

    let mut entries = make_entries(
        std::slice::from_ref(&selected.id),
        &snapshot,
        current_saved_id.as_deref(),
        &ctx,
    );
    let Some(entry) = entries.pop() else {
        return Err(profile_not_found(use_color_stderr()));
    };

    if json {
        return print_current_status_json(Some(entry));
    }

    let lines = render_entries(&[entry], &ctx, false);
    print_output_block(&lines.join("\n"));
    Ok(())
}

fn status_all_profiles(paths: &Paths, json: bool) -> Result<(), String> {
    let snapshot = load_snapshot(paths, false)?;
    let current_saved_id = current_saved_id(paths, &snapshot.tokens);
    let mut ctx = ListCtx::new(paths, true, true, false);
    if json {
        ctx.use_color = false;
    }

    let ordered = ordered_profile_ids(&snapshot, current_saved_id.as_deref());
    let filtered: Vec<String> = ordered
        .into_iter()
        .filter(|id| current_saved_id.as_deref() != Some(id.as_str()))
        .collect();

    let (current_entry, list_entries) = rayon::join(
        || {
            make_current(
                paths,
                current_saved_id.as_deref(),
                &snapshot.labels,
                &snapshot.tokens,
                &ctx,
            )
        },
        || make_entries(&filtered, &snapshot, None, &ctx),
    );

    if json {
        let mut profiles = Vec::new();
        if let Some(entry) = current_entry {
            profiles.push(entry);
        }
        profiles.extend(list_entries);
        return print_all_status_json(profiles);
    }

    if current_entry.is_none() && list_entries.is_empty() {
        let message = format_no_profiles(paths, ctx.use_color);
        print_output_block(&message);
        return Ok(());
    }

    let mut lines = Vec::new();
    if let Some(err) = ctx.base_url_error.as_deref() {
        lines.push(format_error(err));
        if current_entry.is_some() || !list_entries.is_empty() {
            push_separator(&mut lines, true);
        }
    }
    if let Some(entry) = current_entry {
        lines.extend(render_entries(&[entry], &ctx, true));
        if !list_entries.is_empty() {
            push_separator(&mut lines, true);
            lines.push(String::new());
        }
    }

    if !list_entries.is_empty() {
        lines.extend(render_entries(&list_entries, &ctx, true));
    }

    let output = lines.join("\n");
    print_output_block(&output);
    Ok(())
}

pub type Labels = BTreeMap<String, String>;

const PROFILES_INDEX_VERSION: u8 = 3;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ProfilesIndex {
    #[serde(default = "profiles_index_version")]
    version: u8,
    #[serde(default)]
    profiles: BTreeMap<String, ProfileIndexEntry>,
}

impl Default for ProfilesIndex {
    fn default() -> Self {
        Self {
            version: PROFILES_INDEX_VERSION,
            profiles: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProfileIndexEntry {
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    is_api_key: bool,
    #[serde(default)]
    principal_id: Option<String>,
    #[serde(default)]
    workspace_or_org_id: Option<String>,
    #[serde(default)]
    plan_type_key: Option<String>,
}

fn profiles_index_version() -> u8 {
    PROFILES_INDEX_VERSION
}

fn has_legacy_schema(contents: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(contents)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .map(|obj| {
            obj.contains_key("last_used")
                || obj.contains_key("active_profile_id")
                || obj.contains_key("update_cache")
                || obj.contains_key("default_profile_id")
        })
        .unwrap_or(false)
}

pub(crate) fn read_profiles_index(paths: &Paths) -> Result<ProfilesIndex, String> {
    if !paths.profiles_index.exists() {
        return Ok(ProfilesIndex::default());
    }
    let contents = fs::read_to_string(&paths.profiles_index)
        .map_err(|err| crate::msg2(PROFILE_ERR_READ_INDEX, paths.profiles_index.display(), err))?;
    let had_legacy_schema = has_legacy_schema(&contents);
    let mut index: ProfilesIndex = serde_json::from_str(&contents).map_err(|_| {
        crate::msg1(
            PROFILE_ERR_INDEX_INVALID_JSON,
            paths.profiles_index.display(),
        )
    })?;
    if index.version < PROFILES_INDEX_VERSION {
        index.version = PROFILES_INDEX_VERSION;
    }
    if had_legacy_schema {
        let _ = write_profiles_index(paths, &index);
    }
    Ok(index)
}

pub(crate) fn read_profiles_index_relaxed(paths: &Paths) -> ProfilesIndex {
    match read_profiles_index(paths) {
        Ok(index) => index,
        Err(err) => {
            let normalized = normalize_error(&err);
            let warning = format_warning(&normalized, use_color_stderr());
            eprintln!("{warning}");
            ProfilesIndex::default()
        }
    }
}

pub(crate) fn write_profiles_index(paths: &Paths, index: &ProfilesIndex) -> Result<(), String> {
    let json = serde_json::to_string_pretty(index)
        .map_err(|err| crate::msg1(PROFILE_ERR_SERIALIZE_INDEX, err))?;
    crate::common::write_atomic_private(&paths.profiles_index, format!("{json}\n").as_bytes())
        .map_err(|err| crate::msg1(PROFILE_ERR_WRITE_INDEX, err))
}

pub(crate) fn repair_profiles_metadata(paths: &Paths) -> Result<Vec<String>, String> {
    let _lock = lock_usage(paths)?;

    let had_index = paths.profiles_index.exists();
    let mut repairs = Vec::new();
    let mut should_write = false;
    let mut normalized_index = false;
    let mut index = if !had_index {
        should_write = true;
        repairs.push("Initialized profiles index".to_string());
        ProfilesIndex::default()
    } else {
        let contents = fs::read_to_string(&paths.profiles_index).map_err(|err| {
            crate::msg2(PROFILE_ERR_READ_INDEX, paths.profiles_index.display(), err)
        })?;
        let had_legacy_schema = has_legacy_schema(&contents);
        match serde_json::from_str::<ProfilesIndex>(&contents) {
            Ok(mut index) => {
                if index.version < PROFILES_INDEX_VERSION {
                    index.version = PROFILES_INDEX_VERSION;
                    normalized_index = true;
                }
                if had_legacy_schema {
                    normalized_index = true;
                }
                if normalized_index {
                    should_write = true;
                    repairs.push("Normalized profiles index format".to_string());
                }
                index
            }
            Err(_) => {
                should_write = true;
                let backup_path = next_profiles_index_backup_path(&paths.profiles_index);
                write_atomic(&backup_path, contents.as_bytes())?;
                repairs.push(format!(
                    "Backed up invalid profiles index to {}",
                    backup_path.display()
                ));
                repairs.push("Rebuilt invalid profiles index".to_string());
                ProfilesIndex::default()
            }
        }
    };

    let ids = collect_profile_ids(&paths.profiles)?;
    let before_entries = index.profiles.len();

    prune_profiles_index(&mut index, &paths.profiles)?;
    let pruned = before_entries.saturating_sub(index.profiles.len());
    if pruned > 0 {
        should_write = true;
        repairs.push(format!(
            "Pruned {pruned} stale profile index {}",
            if pruned == 1 { "entry" } else { "entries" }
        ));
    }

    let mut indexed = 0usize;
    for id in ids {
        if index.profiles.contains_key(&id) {
            continue;
        }
        let path = profile_path_for_id(&paths.profiles, &id);
        match read_tokens(&path) {
            Ok(tokens) if is_profile_ready(&tokens) => {}
            _ => continue,
        }
        index.profiles.insert(id, ProfileIndexEntry::default());
        indexed += 1;
    }
    if indexed > 0 {
        should_write = true;
        repairs.push(format!(
            "Indexed {indexed} saved {}",
            if indexed == 1 { "profile" } else { "profiles" }
        ));
    }

    if should_write {
        write_profiles_index(paths, &index)?;
    }
    Ok(repairs)
}

fn next_profiles_index_backup_path(path: &Path) -> PathBuf {
    let base = path.with_extension("json.bak");
    if !base.exists() {
        return base;
    }
    let mut idx = 1usize;
    loop {
        let candidate = path.with_extension(format!("json.bak.{idx}"));
        if !candidate.exists() {
            return candidate;
        }
        idx += 1;
    }
}

fn prune_profiles_index(index: &mut ProfilesIndex, profiles_dir: &Path) -> Result<(), String> {
    let ids = collect_profile_ids(profiles_dir)?;
    index.profiles.retain(|id, _| ids.contains(id));
    Ok(())
}

fn sync_profiles_index(index: &mut ProfilesIndex, labels: &Labels) {
    for (id, entry) in index.profiles.iter_mut() {
        entry.label = label_for_id(labels, id);
    }
}

fn labels_from_index(index: &ProfilesIndex) -> Labels {
    let mut labels = Labels::new();
    for (id, entry) in &index.profiles {
        let Some(label) = entry.label.as_deref() else {
            continue;
        };
        let trimmed = label.trim();
        if trimmed.is_empty() || labels.contains_key(trimmed) {
            continue;
        }
        labels.insert(trimmed.to_string(), id.clone());
    }
    labels
}

fn update_profiles_index_entry(
    index: &mut ProfilesIndex,
    id: &str,
    tokens: Option<&Tokens>,
    label: Option<String>,
) {
    let entry = index.profiles.entry(id.to_string()).or_default();
    if let Some(tokens) = tokens {
        let (email, plan) = extract_email_and_plan(tokens);
        entry.email = email;
        entry.plan = plan;
        entry.account_id = token_account_id(tokens).map(str::to_string);
        entry.is_api_key = is_api_key_profile(tokens);
        if let Some(identity) = extract_profile_identity(tokens) {
            entry.principal_id = Some(identity.principal_id);
            entry.workspace_or_org_id = Some(identity.workspace_or_org_id);
            entry.plan_type_key = Some(identity.plan_type);
        }
    }
    if let Some(label) = label {
        entry.label = Some(label);
    }
}

pub fn prune_labels(labels: &mut Labels, profiles_dir: &Path) {
    labels.retain(|_, id| profile_path_for_id(profiles_dir, id).is_file());
}

pub fn assign_label(labels: &mut Labels, label: &str, id: &str) -> Result<(), String> {
    let trimmed = trim_label(label)?;
    if let Some(existing) = labels.get(trimmed)
        && existing != id
    {
        return Err(crate::msg2(
            PROFILE_ERR_LABEL_EXISTS,
            trimmed,
            format_list_hint(use_color_stderr()),
        ));
    }
    remove_labels_for_id(labels, id);
    labels.insert(trimmed.to_string(), id.to_string());
    Ok(())
}

pub fn remove_labels_for_id(labels: &mut Labels, id: &str) {
    labels.retain(|_, value| value != id);
}

pub fn label_for_id(labels: &Labels, id: &str) -> Option<String> {
    labels.iter().find_map(|(label, value)| {
        if value == id {
            Some(label.clone())
        } else {
            None
        }
    })
}

fn labels_by_id(labels: &Labels) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (label, id) in labels {
        out.entry(id.clone()).or_insert_with(|| label.clone());
    }
    out
}

pub fn resolve_label_id(labels: &Labels, label: &str) -> Result<String, String> {
    let trimmed = trim_label(label)?;
    labels.get(trimmed).cloned().ok_or_else(|| {
        crate::msg2(
            PROFILE_ERR_LABEL_NOT_FOUND,
            trimmed,
            format_list_hint(use_color_stderr()),
        )
    })
}

fn resolve_label_target_id(
    store: &ProfileStore,
    label: Option<&str>,
    id: Option<&str>,
) -> Result<String, String> {
    if let Some(label) = label {
        return resolve_label_id(&store.labels, label);
    }

    let Some(id) = id else {
        unreachable!("clap enforces label target selector")
    };
    if store.profiles_index.profiles.contains_key(id) {
        return Ok(id.to_string());
    }
    Err(crate::msg2(
        PROFILE_ERR_ID_NO_MATCH,
        id,
        format_list_hint(use_color_stderr()),
    ))
}

pub fn profile_files(profiles_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    if !profiles_dir.exists() {
        return Ok(files);
    }
    let entries = fs::read_dir(profiles_dir)
        .map_err(|err| crate::msg1(PROFILE_ERR_READ_PROFILES_DIR, err))?;
    for entry in entries {
        let entry = entry.map_err(|err| crate::msg1(PROFILE_ERR_READ_PROFILES_DIR, err))?;
        let path = entry.path();
        if !is_profile_file(&path) {
            continue;
        }
        files.push(path);
    }
    Ok(files)
}

pub fn profile_id_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .filter(|stem| !stem.is_empty())
        .map(|stem| stem.to_string())
}

pub fn profile_path_for_id(profiles_dir: &Path, id: &str) -> PathBuf {
    profiles_dir.join(format!("{id}.json"))
}

pub fn collect_profile_ids(profiles_dir: &Path) -> Result<HashSet<String>, String> {
    let mut ids = HashSet::new();
    for path in profile_files(profiles_dir)? {
        if let Some(stem) = profile_id_from_path(&path) {
            ids.insert(stem);
        }
    }
    Ok(ids)
}

fn resolve_export_ids(
    paths: &Paths,
    store: &ProfileStore,
    label: Option<&str>,
    ids: &[String],
) -> Result<Vec<String>, String> {
    if let Some(label) = label {
        return Ok(vec![resolve_label_target_id(store, Some(label), None)?]);
    }

    let available_ids = collect_profile_ids(&paths.profiles)?;
    if ids.is_empty() {
        let mut all: Vec<String> = available_ids.into_iter().collect();
        all.sort();
        return Ok(all);
    }

    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        if !available_ids.contains(id) {
            return Err(crate::msg2(
                PROFILE_ERR_ID_NO_MATCH,
                id,
                format_list_hint(use_color_stderr()),
            ));
        }
        if seen.insert(id.clone()) {
            selected.push(id.clone());
        }
    }
    Ok(selected)
}

fn prepare_import_profile(profile: ExportedProfile) -> Result<PreparedImportProfile, String> {
    let ExportedProfile {
        id,
        label,
        contents,
    } = profile;
    let mut bytes = serde_json::to_vec_pretty(&contents).map_err(|err| {
        format!(
            "Error: Exported profile '{}' could not be serialized: {err}",
            id
        )
    })?;
    bytes.push(b'\n');

    let auth: AuthFile = serde_json::from_value(contents)
        .map_err(|err| format!("Error: Exported profile '{}' is invalid JSON: {err}", id))?;
    let tokens = if let Some(tokens) = auth.tokens {
        tokens
    } else if let Some(api_key) = auth.openai_api_key.as_deref() {
        tokens_from_api_key(api_key)
    } else {
        return Err(format!(
            "Error: Exported profile '{}' is missing tokens or API key.",
            id
        ));
    };
    if !is_profile_ready(&tokens) {
        return Err(format!("Error: Exported profile '{}' is incomplete.", id));
    }

    Ok(PreparedImportProfile {
        id,
        label,
        contents: bytes,
        tokens,
    })
}

fn validate_import_profile_id(id: &str) -> Result<(), String> {
    let mut components = Path::new(id).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(format!("Error: Imported profile id '{}' is not safe.", id));
    }
    if matches!(id, "profiles" | "update") {
        return Err(format!("Error: Imported profile id '{}' is reserved.", id));
    }
    Ok(())
}

fn cleanup_imported_profiles(paths: &Paths, ids: &[String]) {
    for id in ids {
        let _ = fs::remove_file(profile_path_for_id(&paths.profiles, id));
    }
}

fn tighten_export_permissions(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions).map_err(|err| {
            format!(
                "Error: Could not secure export file {}: {err}",
                path.display()
            )
        })?;
    }
    Ok(())
}

pub fn load_profile_tokens_map(
    paths: &Paths,
) -> Result<BTreeMap<String, Result<Tokens, String>>, String> {
    let mut map = BTreeMap::new();
    for path in profile_files(&paths.profiles)? {
        let Some(stem) = profile_id_from_path(&path) else {
            continue;
        };
        match read_tokens(&path) {
            Ok(tokens) => {
                map.insert(stem, Ok(tokens));
            }
            Err(err) => {
                map.insert(stem, Err(normalize_error(&err)));
            }
        }
    }
    Ok(map)
}

pub(crate) fn resolve_save_id(
    paths: &Paths,
    profiles_index: &mut ProfilesIndex,
    tokens: &Tokens,
) -> Result<String, String> {
    let (_, email, plan) = require_identity(tokens)?;
    let identity =
        extract_profile_identity(tokens).ok_or_else(|| AUTH_ERR_INCOMPLETE_ACCOUNT.to_string())?;
    let (desired_base, desired, candidates) = desired_candidates(paths, &identity, &email, &plan)?;
    if let Some(primary) = pick_primary(&candidates).filter(|primary| primary != &desired) {
        return rename_profile_id(paths, profiles_index, &primary, &desired_base, &identity);
    }
    Ok(desired)
}

pub(crate) fn resolve_sync_id(
    paths: &Paths,
    profiles_index: &mut ProfilesIndex,
    tokens: &Tokens,
) -> Result<Option<String>, String> {
    let Ok((_, email, plan)) = require_identity(tokens) else {
        return Ok(None);
    };
    let Some(identity) = extract_profile_identity(tokens) else {
        return Ok(None);
    };
    let (desired_base, desired, candidates) = desired_candidates(paths, &identity, &email, &plan)?;
    if candidates.len() == 1 {
        return Ok(candidates.first().cloned());
    }
    if candidates.iter().any(|id| id == &desired) {
        return Ok(Some(desired));
    }
    let Some(primary) = pick_primary(&candidates) else {
        return Ok(None);
    };
    if primary != desired {
        let renamed = rename_profile_id(paths, profiles_index, &primary, &desired_base, &identity)?;
        return Ok(Some(renamed));
    }
    Ok(Some(primary))
}

pub(crate) fn cached_profile_ids(
    tokens_map: &BTreeMap<String, Result<Tokens, String>>,
    identity: &ProfileIdentityKey,
) -> Vec<String> {
    tokens_map
        .iter()
        .filter_map(|(id, result)| {
            result
                .as_ref()
                .ok()
                .filter(|tokens| matches_identity(tokens, identity))
                .map(|_| id.clone())
        })
        .collect()
}

pub(crate) fn pick_primary(candidates: &[String]) -> Option<String> {
    candidates.iter().min().cloned()
}

fn desired_candidates(
    paths: &Paths,
    identity: &ProfileIdentityKey,
    email: &str,
    plan: &str,
) -> Result<(String, String, Vec<String>), String> {
    let (desired_base, desired) = desired_id(paths, identity, email, plan);
    let candidates = scan_profile_ids(&paths.profiles, identity)?;
    Ok((desired_base, desired, candidates))
}

fn desired_id(
    paths: &Paths,
    identity: &ProfileIdentityKey,
    email: &str,
    plan: &str,
) -> (String, String) {
    let desired_base = profile_base(email, plan);
    let desired = unique_id(&desired_base, identity, &paths.profiles);
    (desired_base, desired)
}

fn profile_base(email: &str, plan_label: &str) -> String {
    let email = sanitize_part(email);
    let plan = sanitize_part(plan_label);
    let email = if email.is_empty() {
        "unknown".to_string()
    } else {
        email
    };
    let plan = if plan.is_empty() {
        "unknown".to_string()
    } else {
        plan
    };
    format!("{email}-{plan}")
}

fn sanitize_part(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_dash = false;
    for ch in value.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if matches!(ch, '@' | '.' | '-' | '_' | '+') {
            Some(ch)
        } else {
            Some('-')
        };
        if let Some(next) = next {
            if next == '-' {
                if last_dash {
                    continue;
                }
                last_dash = true;
            } else {
                last_dash = false;
            }
            out.push(next);
        }
    }
    out.trim_matches('-').to_string()
}

fn unique_id(base: &str, identity: &ProfileIdentityKey, profiles_dir: &Path) -> String {
    let mut candidate = base.to_string();
    let suffix = short_identity_suffix(identity);
    let mut attempts = 0usize;
    loop {
        let path = profile_path_for_id(profiles_dir, &candidate);
        if !path.is_file() {
            return candidate;
        }
        if read_tokens(&path)
            .ok()
            .is_some_and(|tokens| matches_identity(&tokens, identity))
        {
            return candidate;
        }
        attempts += 1;
        if attempts == 1 {
            candidate = format!("{base}-{suffix}");
        } else {
            candidate = format!("{base}-{suffix}-{attempts}");
        }
    }
}

fn short_identity_suffix(identity: &ProfileIdentityKey) -> String {
    let source = if identity.workspace_or_org_id == "unknown" {
        identity.principal_id.as_str()
    } else {
        identity.workspace_or_org_id.as_str()
    };
    let suffix: String = source.chars().take(6).collect();
    if suffix.is_empty() {
        "id".to_string()
    } else {
        suffix
    }
}

fn scan_profile_ids(
    profiles_dir: &Path,
    identity: &ProfileIdentityKey,
) -> Result<Vec<String>, String> {
    let mut matches = Vec::new();
    for path in profile_files(profiles_dir)? {
        let Ok(tokens) = read_tokens(&path) else {
            continue;
        };
        if !matches_identity(&tokens, identity) {
            continue;
        }
        if let Some(stem) = profile_id_from_path(&path) {
            matches.push(stem);
        }
    }
    Ok(matches)
}

fn matches_identity(tokens: &Tokens, identity: &ProfileIdentityKey) -> bool {
    extract_profile_identity(tokens).is_some_and(|candidate| candidate == *identity)
}

fn rename_profile_id(
    paths: &Paths,
    profiles_index: &mut ProfilesIndex,
    from: &str,
    target_base: &str,
    identity: &ProfileIdentityKey,
) -> Result<String, String> {
    let desired = unique_id(target_base, identity, &paths.profiles);
    if from == desired {
        return Ok(desired);
    }
    let from_path = profile_path_for_id(&paths.profiles, from);
    let to_path = profile_path_for_id(&paths.profiles, &desired);
    if !from_path.is_file() {
        return Err(crate::msg1(PROFILE_ERR_ID_NOT_FOUND, from));
    }
    fs::rename(&from_path, &to_path)
        .map_err(|err| crate::msg2(PROFILE_ERR_RENAME_PROFILE, from, err))?;
    if let Some(entry) = profiles_index.profiles.remove(from) {
        profiles_index.profiles.insert(desired.clone(), entry);
    }
    Ok(desired)
}

pub(crate) struct Snapshot {
    pub(crate) labels: Labels,
    pub(crate) tokens: BTreeMap<String, Result<Tokens, String>>,
    pub(crate) index: ProfilesIndex,
}

pub(crate) fn sync_current(paths: &Paths, index: &mut ProfilesIndex) -> Result<(), String> {
    let Some(tokens) = read_tokens_opt(&paths.auth) else {
        return Ok(());
    };
    let id = match resolve_sync_id(paths, index, &tokens)? {
        Some(id) => id,
        None => return Ok(()),
    };
    let target = profile_path_for_id(&paths.profiles, &id);
    sync_profile(paths, &target)?;
    let label = label_for_id(&labels_from_index(index), &id);
    update_profiles_index_entry(index, &id, Some(&tokens), label);
    Ok(())
}

fn sync_profile(paths: &Paths, target: &Path) -> Result<(), String> {
    copy_atomic(&paths.auth, target).map_err(|err| crate::msg1(PROFILE_ERR_SYNC_CURRENT, err))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(target, fs::Permissions::from_mode(0o600))
            .map_err(|err| crate::msg1(PROFILE_ERR_SYNC_CURRENT, err))?;
    }
    Ok(())
}

pub(crate) fn load_snapshot(paths: &Paths, strict_labels: bool) -> Result<Snapshot, String> {
    let _lock = lock_usage(paths)?;
    let tokens = load_profile_tokens_map(paths)?;
    let ids: HashSet<String> = tokens.keys().cloned().collect();
    let mut index = if strict_labels {
        read_profiles_index(paths)?
    } else {
        read_profiles_index_relaxed(paths)
    };
    let _ = prune_profiles_index(&mut index, &paths.profiles);
    for id in &ids {
        index.profiles.entry(id.clone()).or_default();
    }
    let labels = labels_from_index(&index);

    Ok(Snapshot {
        labels,
        tokens,
        index,
    })
}

pub(crate) fn unsaved_reason(
    paths: &Paths,
    tokens_map: &BTreeMap<String, Result<Tokens, String>>,
) -> Option<String> {
    let tokens = read_tokens_opt(&paths.auth)?;
    let identity = extract_profile_identity(&tokens)?;
    let candidates = cached_profile_ids(tokens_map, &identity);
    if candidates.is_empty() {
        return Some(PROFILE_UNSAVED_NO_MATCH.to_string());
    }
    None
}

pub(crate) fn current_saved_id(
    paths: &Paths,
    tokens_map: &BTreeMap<String, Result<Tokens, String>>,
) -> Option<String> {
    let tokens = read_tokens_opt(&paths.auth)?;
    let identity = extract_profile_identity(&tokens)?;
    let candidates = cached_profile_ids(tokens_map, &identity);
    pick_primary(&candidates)
}

pub(crate) struct ProfileStore {
    _lock: UsageLock,
    pub(crate) labels: Labels,
    pub(crate) profiles_index: ProfilesIndex,
}

impl ProfileStore {
    pub(crate) fn load(paths: &Paths) -> Result<Self, String> {
        let lock = lock_usage(paths)?;
        let mut profiles_index = read_profiles_index_relaxed(paths);
        let _ = prune_profiles_index(&mut profiles_index, &paths.profiles);
        let ids = collect_profile_ids(&paths.profiles)?;
        for id in &ids {
            profiles_index.profiles.entry(id.clone()).or_default();
        }
        let labels = labels_from_index(&profiles_index);
        Ok(Self {
            _lock: lock,
            labels,
            profiles_index,
        })
    }

    pub(crate) fn save(&mut self, paths: &Paths) -> Result<(), String> {
        prune_labels(&mut self.labels, &paths.profiles);
        prune_profiles_index(&mut self.profiles_index, &paths.profiles)?;
        sync_profiles_index(&mut self.profiles_index, &self.labels);
        write_profiles_index(paths, &self.profiles_index)?;
        Ok(())
    }
}

fn profile_not_found(use_color: bool) -> String {
    crate::msg1(PROFILE_MSG_NOT_FOUND, format_list_hint(use_color))
}

fn load_snapshot_ordered(
    paths: &Paths,
    strict_labels: bool,
    no_profiles_message: &str,
) -> Result<(Snapshot, Vec<String>), String> {
    let snapshot = load_snapshot(paths, strict_labels)?;
    let current_saved = current_saved_id(paths, &snapshot.tokens);
    let ordered = ordered_profile_ids(&snapshot, current_saved.as_deref());
    if ordered.is_empty() {
        return Err(no_profiles_message.to_string());
    }
    Ok((snapshot, ordered))
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ProfileOrderKey {
    current_rank: u8,
    label_missing: bool,
    label: String,
    email_missing: bool,
    email: String,
    id: String,
}

fn ordered_profile_ids(snapshot: &Snapshot, current_saved_id: Option<&str>) -> Vec<String> {
    let labels_by_id = labels_by_id(&snapshot.labels);
    let mut keyed: Vec<(String, ProfileOrderKey)> = snapshot
        .tokens
        .keys()
        .cloned()
        .map(|id| {
            let label = labels_by_id
                .get(&id)
                .cloned()
                .or_else(|| {
                    snapshot
                        .index
                        .profiles
                        .get(&id)
                        .and_then(|entry| entry.label.clone())
                })
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            let email = snapshot
                .tokens
                .get(&id)
                .and_then(|result| result.as_ref().ok())
                .and_then(|tokens| extract_email_and_plan(tokens).0)
                .or_else(|| {
                    snapshot
                        .index
                        .profiles
                        .get(&id)
                        .and_then(|entry| entry.email.clone())
                })
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            let key = ProfileOrderKey {
                current_rank: if current_saved_id == Some(id.as_str()) {
                    0
                } else {
                    1
                },
                label_missing: label.is_empty(),
                label,
                email_missing: email.is_empty(),
                email,
                id: id.to_ascii_lowercase(),
            };
            (id, key)
        })
        .collect();
    keyed.sort_by(|left, right| left.1.cmp(&right.1));
    keyed.into_iter().map(|(id, _)| id).collect()
}

fn copy_profile(source: &Path, dest: &Path, context: &str) -> Result<(), String> {
    copy_atomic(source, dest)
        .map_err(|err| crate::msg3(PROFILE_ERR_COPY_CONTEXT, context, dest.display(), err))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dest, fs::Permissions::from_mode(0o600))
            .map_err(|err| crate::msg3(PROFILE_ERR_COPY_CONTEXT, context, dest.display(), err))?;
    }
    Ok(())
}

fn make_candidates(paths: &Paths, snapshot: &Snapshot, ordered: &[String]) -> Vec<Candidate> {
    let current_saved = current_saved_id(paths, &snapshot.tokens);
    build_candidates(ordered, snapshot, current_saved.as_deref())
}

fn pick_one(
    action: &str,
    label: Option<&str>,
    id: Option<&str>,
    snapshot: &Snapshot,
    candidates: &[Candidate],
) -> Result<Candidate, String> {
    if let Some(label) = label {
        select_by_label(label, &snapshot.labels, candidates)
    } else if let Some(id) = id {
        select_by_id(id, candidates)
    } else if !io::stdin().is_terminal() {
        require_tty(action)?;
        unreachable!("require_tty should always return Err in non-interactive mode")
    } else {
        select_single_profile("", candidates)
    }
}

fn pick_many(
    action: &str,
    label: Option<&str>,
    ids: &[String],
    snapshot: &Snapshot,
    candidates: &[Candidate],
) -> Result<Vec<Candidate>, String> {
    if let Some(label) = label {
        Ok(vec![select_by_label(label, &snapshot.labels, candidates)?])
    } else if !ids.is_empty() {
        select_many_by_id(ids, candidates)
    } else {
        require_tty(action)?;
        select_multiple_profiles("", candidates)
    }
}

pub(crate) struct ProfileInfo {
    pub(crate) display: String,
    pub(crate) email: Option<String>,
    pub(crate) plan: Option<String>,
    pub(crate) is_free: bool,
}

pub(crate) fn profile_info(
    tokens: Option<&Tokens>,
    label: Option<String>,
    is_current: bool,
    use_color: bool,
) -> ProfileInfo {
    profile_info_with_fallback(tokens, None, label, is_current, use_color)
}

fn profile_info_with_fallback(
    tokens: Option<&Tokens>,
    fallback: Option<&ProfileIndexEntry>,
    label: Option<String>,
    is_current: bool,
    use_color: bool,
) -> ProfileInfo {
    let (email, plan) = if let Some(tokens) = tokens {
        extract_email_and_plan(tokens)
    } else if let Some(entry) = fallback {
        (entry.email.clone(), entry.plan.clone())
    } else {
        (None, None)
    };
    let is_free = is_free_plan(plan.as_deref());
    let display =
        crate::format_profile_display(email.clone(), plan.clone(), label, is_current, use_color);
    ProfileInfo {
        display,
        email,
        plan,
        is_free,
    }
}

#[derive(Debug)]
pub(crate) enum LoadChoice {
    SaveAndContinue,
    ContinueWithoutSaving,
    Cancel,
}

pub(crate) fn prompt_unsaved_load(paths: &Paths, reason: &str) -> Result<LoadChoice, String> {
    let is_tty = io::stdin().is_terminal();
    if !is_tty {
        let hint = format_save_before_load_or_force(paths, use_color_stderr());
        return Err(crate::msg1(PROFILE_ERR_CURRENT_NOT_SAVED, hint));
    }
    let selection = Select::new(
        "",
        vec![
            PROFILE_PROMPT_SAVE_AND_CONTINUE,
            PROFILE_PROMPT_CONTINUE_WITHOUT_SAVING,
            PROFILE_PROMPT_CANCEL,
        ],
    )
    .with_render_config(inquire_select_render_config())
    .prompt();
    prompt_unsaved_load_with(paths, reason, is_tty, selection)
}

fn prompt_unsaved_load_with(
    paths: &Paths,
    reason: &str,
    is_tty: bool,
    selection: Result<&str, inquire::error::InquireError>,
) -> Result<LoadChoice, String> {
    if !is_tty {
        let hint = format_save_before_load_or_force(paths, use_color_stderr());
        return Err(crate::msg1(PROFILE_ERR_CURRENT_NOT_SAVED, hint));
    }
    let warning = format_warning(
        &crate::msg1(PROFILE_WARN_CURRENT_NOT_SAVED_REASON, reason),
        use_color_stderr(),
    );
    eprintln!("{warning}");
    match selection {
        Ok(PROFILE_PROMPT_SAVE_AND_CONTINUE) => Ok(LoadChoice::SaveAndContinue),
        Ok(PROFILE_PROMPT_CONTINUE_WITHOUT_SAVING) => Ok(LoadChoice::ContinueWithoutSaving),
        Ok(_) => Ok(LoadChoice::Cancel),
        Err(err) if is_inquire_cancel(&err) => Ok(LoadChoice::Cancel),
        Err(err) => Err(crate::msg1(PROFILE_ERR_PROMPT_LOAD, err)),
    }
}

pub(crate) fn build_candidates(
    ordered: &[String],
    snapshot: &Snapshot,
    current_saved_id: Option<&str>,
) -> Vec<Candidate> {
    let mut candidates = Vec::with_capacity(ordered.len());
    let use_color = use_color_stderr();
    let labels_by_id = labels_by_id(&snapshot.labels);
    for id in ordered {
        let label = labels_by_id.get(id).cloned();
        let tokens = snapshot
            .tokens
            .get(id)
            .and_then(|result| result.as_ref().ok());
        let index_entry = snapshot.index.profiles.get(id);
        let is_current = current_saved_id == Some(id.as_str());
        let info = profile_info_with_fallback(tokens, index_entry, label, is_current, use_color);
        let marker = if is_current {
            current_profile_marker(use_color)
        } else {
            String::new()
        };
        candidates.push(Candidate {
            id: id.clone(),
            display: format!("{}{}", info.display, marker),
        });
    }
    candidates
}

pub(crate) fn require_tty(action: &str) -> Result<(), String> {
    require_tty_with(io::stdin().is_terminal(), action)
}

fn require_tty_with(is_tty: bool, action: &str) -> Result<(), String> {
    if is_tty {
        Ok(())
    } else {
        Err(crate::msg3(
            PROFILE_ERR_TTY_REQUIRED,
            action,
            command_name(),
            action,
        ))
    }
}

pub(crate) fn select_single_profile(
    title: &str,
    candidates: &[Candidate],
) -> Result<Candidate, String> {
    let options = candidates.to_vec();
    let render_config = inquire_select_render_config();
    let prompt = Select::new(title, options)
        .with_help_message(PROFILE_LOAD_HELP)
        .with_render_config(render_config)
        .prompt();
    handle_inquire_result(prompt, "selection")
}

pub(crate) fn select_multiple_profiles(
    title: &str,
    candidates: &[Candidate],
) -> Result<Vec<Candidate>, String> {
    let options = candidates.to_vec();
    let render_config = inquire_select_render_config();
    let prompt = MultiSelect::new(title, options)
        .with_help_message(PROFILE_DELETE_HELP)
        .with_render_config(render_config)
        .prompt();
    let selections = handle_inquire_result(prompt, "selection")?;
    if selections.is_empty() {
        return Err(CANCELLED_MESSAGE.to_string());
    }
    Ok(selections)
}

pub(crate) fn select_by_label(
    label: &str,
    labels: &Labels,
    candidates: &[Candidate],
) -> Result<Candidate, String> {
    let id = resolve_label_id(labels, label)?;
    let Some(candidate) = candidates.iter().find(|candidate| candidate.id == id) else {
        return Err(crate::msg2(
            PROFILE_ERR_LABEL_NO_MATCH,
            label,
            format_list_hint(use_color_stderr()),
        ));
    };
    Ok(candidate.clone())
}

pub(crate) fn select_by_id(id: &str, candidates: &[Candidate]) -> Result<Candidate, String> {
    let Some(candidate) = candidates.iter().find(|candidate| candidate.id == id) else {
        return Err(crate::msg2(
            PROFILE_ERR_ID_NO_MATCH,
            id,
            format_list_hint(use_color_stderr()),
        ));
    };
    Ok(candidate.clone())
}

fn select_many_by_id(ids: &[String], candidates: &[Candidate]) -> Result<Vec<Candidate>, String> {
    let mut selections = Vec::with_capacity(ids.len());
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        selections.push(select_by_id(id, candidates)?);
    }
    Ok(selections)
}

pub(crate) fn confirm_delete_profiles(displays: &[String]) -> Result<bool, String> {
    let is_tty = io::stdin().is_terminal();
    if !is_tty {
        return Err(PROFILE_ERR_DELETE_CONFIRM_REQUIRED.to_string());
    }
    let prompt = if displays.len() == 1 {
        crate::msg1(PROFILE_PROMPT_DELETE_ONE, &displays[0])
    } else {
        let count = displays.len();
        eprintln!("{}", crate::msg1(PROFILE_PROMPT_DELETE_MANY, count));
        for display in displays {
            eprintln!(" - {display}");
        }
        PROFILE_PROMPT_DELETE_SELECTED.to_string()
    };
    let selection = Confirm::new(&prompt)
        .with_default(false)
        .with_render_config(inquire_select_render_config())
        .prompt();
    confirm_delete_profiles_with(is_tty, selection)
}

fn confirm_delete_profiles_with(
    is_tty: bool,
    selection: Result<bool, inquire::error::InquireError>,
) -> Result<bool, String> {
    if !is_tty {
        return Err(PROFILE_ERR_DELETE_CONFIRM_REQUIRED.to_string());
    }
    match selection {
        Ok(value) => Ok(value),
        Err(err) if is_inquire_cancel(&err) => Err(CANCELLED_MESSAGE.to_string()),
        Err(err) => Err(crate::msg1(PROFILE_ERR_PROMPT_DELETE, err)),
    }
}

#[derive(Clone)]
pub(crate) struct Candidate {
    pub(crate) id: String,
    pub(crate) display: String,
}

impl fmt::Display for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header = format_entry_header(&self.display, use_color_stderr());
        write!(f, "{header}")
    }
}

fn render_entries(entries: &[Entry], ctx: &ListCtx, allow_plain_spacing: bool) -> Vec<String> {
    let mut lines = Vec::with_capacity((entries.len().max(1)) * 4);
    for (idx, entry) in entries.iter().enumerate() {
        let mut entry_lines = Vec::new();
        let mut header = format_entry_header(&entry.display, ctx.use_color);
        if ctx.show_id
            && let Some(id) = entry.id.as_deref()
        {
            header.push_str(&format_profile_id_suffix(id, ctx.use_color));
        }
        if ctx.show_current_marker && entry.is_current {
            header.push_str(&current_profile_marker(ctx.use_color));
        }
        let show_detail_lines = ctx.show_usage || entry.always_show_details;
        if !show_detail_lines {
            if let Some(err) = entry.error_summary.as_deref() {
                header.push_str(&format!("  {err}"));
                entry_lines.push(header);
            } else {
                entry_lines.push(header);
            }
        } else {
            entry_lines.push(header);
            entry_lines.push(String::new());
            entry_lines.extend(render_entry_details(entry));
        }
        lines.extend(entry_lines);
        if idx + 1 < entries.len() {
            push_separator(&mut lines, allow_plain_spacing);
            if ctx.show_usage && allow_plain_spacing {
                lines.push(String::new());
            }
        }
    }
    lines
}

fn render_entry_details(entry: &Entry) -> Vec<String> {
    let mut details = Vec::new();
    for line in &entry.details {
        if line.is_empty() {
            details.push(String::new());
            continue;
        }
        details.extend(line.lines().enumerate().map(|(index, part)| {
            if part.is_empty() {
                String::new()
            } else if index == 0 {
                format!(" {part}")
            } else {
                part.to_string()
            }
        }));
    }
    details
}

fn push_separator(lines: &mut Vec<String>, allow_plain_spacing: bool) {
    if !is_plain() || allow_plain_spacing {
        lines.push(String::new());
    }
}

fn current_profile_marker(use_color: bool) -> String {
    style_text(" <- active", use_color, |text| text.dimmed().italic())
}

fn format_profile_id_suffix(id: &str, use_color: bool) -> String {
    style_text(&format!(" [id: {id}]"), use_color, |text| text.dimmed())
}

fn make_error(
    id: Option<String>,
    label: Option<String>,
    index_entry: Option<&ProfileIndexEntry>,
    use_color: bool,
    message: &str,
    summary_label: &str,
    is_current: bool,
) -> Entry {
    let info = profile_info_with_fallback(None, index_entry, label.clone(), is_current, use_color);
    let is_saved = id.is_some();
    Entry {
        id,
        label,
        email: info.email.clone(),
        plan: info.plan.clone(),
        is_api_key: index_entry.map(|entry| entry.is_api_key).unwrap_or(false),
        is_saved,
        display: info.display,
        details: vec![format_error(message)],
        warnings: Vec::new(),
        usage: None,
        error_summary: Some(error_summary(summary_label, message)),
        always_show_details: false,
        is_current,
    }
}

fn unavailable_lines(message: &str, use_color: bool) -> Vec<String> {
    let (summary, detail) = usage_message_parts(message);
    let mut lines = vec![format_usage_unavailable(&summary, use_color)];
    if let Some(detail) = detail {
        lines.extend(
            detail
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| format!("      {line}")),
        );
    }
    lines
}

fn plain_error_lines(message: &str, use_color: bool) -> Vec<String> {
    let mut lines = message.lines();
    let Some(first) = lines.next() else {
        return Vec::new();
    };

    let mut headline = first.to_string();
    let mut tail: Vec<String> = lines.map(str::to_string).collect();
    let mut merged_status = false;
    if let Some(second) = tail.first() {
        let second = second.trim();
        if second.starts_with("unexpected status ") {
            headline = format!("{headline} ({second})");
            tail.remove(0);
            merged_status = true;
        }
    }

    let mut rendered = vec![format_error(&headline)];
    rendered.extend(tail.into_iter().enumerate().map(|(index, line)| {
        let adjusted_index = if merged_status { index + 1 } else { index };
        let text = if adjusted_index == 0 {
            line
        } else {
            format!(" {line}")
        };
        if adjusted_index == 0 {
            text
        } else {
            crate::ui::style_text(&text, use_color, |text| text.dimmed())
        }
    }));
    rendered
}

fn usage_message_parts(message: &str) -> (String, Option<String>) {
    let normalized = normalize_error(message);
    let mut lines = normalized
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let summary = lines.next().unwrap_or_default().to_string();
    let detail_lines: Vec<&str> = lines.collect();
    let detail = if detail_lines.is_empty() {
        None
    } else {
        Some(detail_lines.join("\n"))
    };
    (summary, detail)
}

#[derive(Clone, Serialize)]
struct StatusUsageJson {
    state: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    buckets: Vec<crate::usage::UsageSnapshotBucket>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl StatusUsageJson {
    fn ok(buckets: Vec<crate::usage::UsageSnapshotBucket>) -> Self {
        Self {
            state: "ok",
            buckets,
            status_code: None,
            summary: None,
            detail: None,
        }
    }

    fn from_message(state: &'static str, status_code: Option<u16>, message: &str) -> Self {
        let (summary, detail) = usage_message_parts(message);
        Self {
            state,
            buckets: Vec::new(),
            status_code,
            summary: Some(summary),
            detail,
        }
    }

    fn from_fetch_error(err: &crate::usage::UsageFetchError) -> Self {
        Self::from_message("error", err.status_code(), &err.message())
    }

    fn unavailable(message: &str) -> Self {
        Self::from_message("unavailable", None, message)
    }
}

fn detail_lines(
    tokens: &mut Tokens,
    email: Option<&str>,
    plan: Option<&str>,
    ctx: &ListCtx,
    source_path: &Path,
) -> (Vec<String>, Option<String>, Option<StatusUsageJson>, bool) {
    let use_color = ctx.use_color;
    let initial_account_id = token_account_id(tokens).map(str::to_string);
    let access_token = tokens.access_token.clone();
    if is_api_key_profile(tokens) {
        if ctx.show_usage {
            let message = crate::msg2(
                UI_ERROR_TWO_LINE,
                USAGE_UNAVAILABLE_API_KEY_TITLE,
                USAGE_UNAVAILABLE_API_KEY_DETAIL,
            );
            return (
                vec![format_error(&message)],
                None,
                Some(StatusUsageJson::unavailable(&message)),
                false,
            );
        }
        return (Vec::new(), None, None, false);
    }
    let unavailable_text = usage_unavailable();
    if let Some(message) = profile_error(tokens, email, plan) {
        let missing_access = access_token.is_none() || initial_account_id.is_none();
        let missing_identity_only =
            message == AUTH_ERR_PROFILE_MISSING_EMAIL_PLAN && !missing_access;
        if !missing_identity_only {
            if ctx.show_usage && missing_access && email.is_some() && plan.is_some() {
                return (
                    unavailable_lines(unavailable_text, use_color),
                    None,
                    Some(StatusUsageJson::unavailable(unavailable_text)),
                    false,
                );
            }
            let details = vec![format_error(message)];
            let summary = Some(error_summary(PROFILE_SUMMARY_ERROR, message));
            return (
                details,
                summary,
                Some(StatusUsageJson::from_message("error", None, message)),
                false,
            );
        }
    }
    if ctx.show_usage {
        if let Some(err) = ctx.base_url_error.as_deref() {
            return (
                vec![format_error(err)],
                Some(error_summary(PROFILE_SUMMARY_USAGE_ERROR, err)),
                Some(StatusUsageJson::from_message("error", None, err)),
                false,
            );
        }
        let Some(base_url) = ctx.base_url.as_deref() else {
            return (Vec::new(), None, None, false);
        };
        let Some(access_token) = access_token.as_deref() else {
            return (Vec::new(), None, None, false);
        };
        let Some(account_id) = initial_account_id.as_deref() else {
            return (Vec::new(), None, None, false);
        };
        match crate::usage::fetch_usage_status(
            base_url,
            access_token,
            account_id,
            unavailable_text,
            ctx.now,
        ) {
            Ok((details, buckets)) => (details, None, Some(StatusUsageJson::ok(buckets)), false),
            Err(err) if err.status_code() == Some(401) => {
                match crate::auth::refresh_profile_tokens(source_path, tokens) {
                    Ok(()) => {
                        let Some(access_token) = tokens.access_token.as_deref() else {
                            let message = AUTH_ERR_INCOMPLETE_ACCOUNT;
                            return (
                                vec![format_error(message)],
                                Some(error_summary(PROFILE_SUMMARY_AUTH_ERROR, message)),
                                Some(StatusUsageJson::from_message("error", None, message)),
                                true,
                            );
                        };
                        let Some(account_id) =
                            token_account_id(tokens).or(initial_account_id.as_deref())
                        else {
                            let message = AUTH_ERR_INCOMPLETE_ACCOUNT;
                            return (
                                vec![format_error(message)],
                                Some(error_summary(PROFILE_SUMMARY_AUTH_ERROR, message)),
                                Some(StatusUsageJson::from_message("error", None, message)),
                                true,
                            );
                        };
                        match crate::usage::fetch_usage_status(
                            base_url,
                            access_token,
                            account_id,
                            unavailable_text,
                            ctx.now,
                        ) {
                            Ok((details, buckets)) => {
                                (details, None, Some(StatusUsageJson::ok(buckets)), true)
                            }
                            Err(err) if err.status_code() == Some(401) => (
                                plain_error_lines(&err.plain_message(), use_color),
                                Some(error_summary(PROFILE_SUMMARY_AUTH_ERROR, &err.message())),
                                Some(StatusUsageJson::from_fetch_error(&err)),
                                true,
                            ),
                            Err(err) => (
                                plain_error_lines(&err.plain_message(), use_color),
                                Some(error_summary(PROFILE_SUMMARY_USAGE_ERROR, &err.message())),
                                Some(StatusUsageJson::from_fetch_error(&err)),
                                true,
                            ),
                        }
                    }
                    Err(err) => (
                        vec![format_error(&err)],
                        Some(error_summary(PROFILE_SUMMARY_AUTH_ERROR, &err)),
                        Some(StatusUsageJson::from_message("error", None, &err)),
                        false,
                    ),
                }
            }
            Err(err) => (
                plain_error_lines(&err.plain_message(), use_color),
                Some(error_summary(PROFILE_SUMMARY_USAGE_ERROR, &err.message())),
                Some(StatusUsageJson::from_fetch_error(&err)),
                false,
            ),
        }
    } else {
        (Vec::new(), None, None, false)
    }
}

#[cfg(test)]
fn is_http_401_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("(401)") || message.contains("unauthorized")
}

fn make_entry(
    label: Option<String>,
    tokens_result: Option<&Result<Tokens, String>>,
    index_entry: Option<&ProfileIndexEntry>,
    profile_path: &Path,
    ctx: &ListCtx,
    is_current: bool,
) -> Entry {
    let use_color = ctx.use_color;
    let label_for_error = label.clone().or_else(|| profile_id_from_path(profile_path));
    let mut tokens = match tokens_result {
        Some(Ok(tokens)) => tokens.clone(),
        Some(Err(err)) => {
            return make_error(
                profile_id_from_path(profile_path),
                label_for_error,
                index_entry,
                use_color,
                err,
                PROFILE_SUMMARY_ERROR,
                is_current,
            );
        }
        None => {
            return make_error(
                profile_id_from_path(profile_path),
                label_for_error,
                index_entry,
                use_color,
                PROFILE_SUMMARY_FILE_MISSING,
                PROFILE_SUMMARY_ERROR,
                is_current,
            );
        }
    };
    let label_value = label.clone();
    let info = profile_info(Some(&tokens), label, is_current, use_color);
    let is_api_key = is_api_key_profile(&tokens);
    let (details, summary, usage, _) = detail_lines(
        &mut tokens,
        info.email.as_deref(),
        info.plan.as_deref(),
        ctx,
        profile_path,
    );
    Entry {
        id: profile_id_from_path(profile_path),
        label: label_value,
        email: info.email,
        plan: info.plan,
        is_api_key,
        is_saved: true,
        display: info.display,
        details,
        warnings: Vec::new(),
        usage,
        error_summary: summary,
        always_show_details: info.is_free,
        is_current,
    }
}

fn make_saved(
    id: &str,
    snapshot: &Snapshot,
    labels_by_id: &BTreeMap<String, String>,
    current_saved_id: Option<&str>,
    ctx: &ListCtx,
) -> Entry {
    let profile_path = ctx.profiles_dir.join(format!("{id}.json"));
    let label = labels_by_id.get(id).cloned();
    let is_current = current_saved_id == Some(id);
    make_entry(
        label,
        snapshot.tokens.get(id),
        snapshot.index.profiles.get(id),
        &profile_path,
        ctx,
        is_current,
    )
}

fn make_entries(
    ordered: &[String],
    snapshot: &Snapshot,
    current_saved_id: Option<&str>,
    ctx: &ListCtx,
) -> Vec<Entry> {
    let labels_by_id = labels_by_id(&snapshot.labels);
    let build = |id: &String| make_saved(id, snapshot, &labels_by_id, current_saved_id, ctx);
    if ctx.show_usage && ordered.len() >= 3 {
        let workers = usage_concurrency().min(ordered.len());
        if workers <= 1 {
            return ordered.iter().map(build).collect();
        }
        if let Ok(pool) = rayon::ThreadPoolBuilder::new().num_threads(workers).build() {
            let mut indexed: Vec<(usize, Entry)> = pool.install(|| {
                ordered
                    .par_iter()
                    .enumerate()
                    .map(|(idx, id)| (idx, build(id)))
                    .collect()
            });
            indexed.sort_by_key(|(idx, _)| *idx);
            return indexed.into_iter().map(|(_, entry)| entry).collect();
        }
        return ordered.iter().map(build).collect();
    }

    ordered.iter().map(build).collect()
}

fn usage_concurrency() -> usize {
    env::var(USAGE_CONCURRENCY_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.clamp(1, MAX_USAGE_CONCURRENCY))
        .unwrap_or(DEFAULT_USAGE_CONCURRENCY)
}

fn make_current(
    paths: &Paths,
    current_saved_id: Option<&str>,
    labels: &Labels,
    tokens_map: &BTreeMap<String, Result<Tokens, String>>,
    ctx: &ListCtx,
) -> Option<Entry> {
    if !paths.auth.is_file() {
        return None;
    }
    let mut tokens = match read_tokens(&paths.auth) {
        Ok(tokens) => tokens,
        Err(err) => {
            return Some(make_error(
                None,
                None,
                None,
                ctx.use_color,
                &err,
                PROFILE_SUMMARY_ERROR,
                true,
            ));
        }
    };
    let resolved_saved_id = extract_profile_identity(&tokens).and_then(|identity| {
        let candidates = cached_profile_ids(tokens_map, &identity);
        pick_primary(&candidates)
    });
    let effective_saved_id = current_saved_id.or(resolved_saved_id.as_deref());
    let label = effective_saved_id.and_then(|id| label_for_id(labels, id));
    let use_color = ctx.use_color;
    let label_value = label.clone();
    let info = profile_info(Some(&tokens), label, true, use_color);
    let plan_is_free = info.is_free;
    let is_api_key = is_api_key_profile(&tokens);
    let can_save = is_profile_ready(&tokens);
    let is_unsaved = effective_saved_id.is_none() && can_save;
    let (mut details, mut summary, mut usage, refreshed) = detail_lines(
        &mut tokens,
        info.email.as_deref(),
        info.plan.as_deref(),
        ctx,
        &paths.auth,
    );
    if refreshed && let Some(saved_id) = effective_saved_id {
        let target = profile_path_for_id(&ctx.profiles_dir, saved_id);
        if let Err(err) = sync_profile(paths, &target) {
            details = vec![format_error(&err)];
            summary = Some(error_summary(PROFILE_SUMMARY_ERROR, &err));
            usage = Some(StatusUsageJson::from_message("error", None, &err));
        }
    }

    let warnings = if is_unsaved {
        format_unsaved_warning(false)
    } else {
        Vec::new()
    };

    if is_unsaved {
        if use_color {
            details.extend(format_unsaved_warning(true));
        } else {
            details.extend(warnings.clone());
        }
    }

    Some(Entry {
        id: effective_saved_id.map(str::to_string),
        label: label_value,
        email: info.email,
        plan: info.plan,
        is_api_key,
        is_saved: effective_saved_id.is_some(),
        display: info.display,
        details,
        warnings,
        usage,
        error_summary: summary,
        always_show_details: is_unsaved || (plan_is_free && !ctx.show_usage),
        is_current: true,
    })
}

fn error_summary(label: &str, message: &str) -> String {
    let (summary, _) = usage_message_parts(message);
    format!("{label}: {summary}")
}

struct ListCtx {
    base_url: Option<String>,
    base_url_error: Option<String>,
    now: DateTime<Local>,
    show_usage: bool,
    show_current_marker: bool,
    show_id: bool,
    use_color: bool,
    profiles_dir: PathBuf,
}

impl ListCtx {
    fn new(paths: &Paths, show_usage: bool, show_current_marker: bool, show_id: bool) -> Self {
        let (base_url, base_url_error) = if show_usage {
            match read_base_url(paths) {
                Ok(url) => (Some(url), None),
                Err(err) => (None, Some(err)),
            }
        } else {
            (None, None)
        };

        Self {
            base_url,
            base_url_error,
            now: Local::now(),
            show_usage,
            show_current_marker,
            show_id,
            use_color: use_color_stdout(),
            profiles_dir: paths.profiles.clone(),
        }
    }
}

#[derive(Clone)]
struct Entry {
    id: Option<String>,
    label: Option<String>,
    email: Option<String>,
    plan: Option<String>,
    is_api_key: bool,
    is_saved: bool,
    display: String,
    details: Vec<String>,
    warnings: Vec<String>,
    usage: Option<StatusUsageJson>,
    error_summary: Option<String>,
    always_show_details: bool,
    is_current: bool,
}

#[derive(Serialize)]
struct ListedProfile {
    id: Option<String>,
    label: Option<String>,
    email: Option<String>,
    plan: Option<String>,
    is_current: bool,
    is_saved: bool,
    is_api_key: bool,
    error: Option<String>,
}

#[derive(Serialize)]
struct ListedProfiles {
    profiles: Vec<ListedProfile>,
}

#[derive(Serialize)]
struct StatusProfileJson {
    id: Option<String>,
    label: Option<String>,
    email: Option<String>,
    plan: Option<String>,
    is_current: bool,
    is_saved: bool,
    is_api_key: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    warnings: Vec<String>,
    usage: Option<StatusUsageJson>,
    error: Option<StatusErrorJson>,
}

#[derive(Serialize)]
struct StatusErrorJson {
    summary: StatusErrorSummaryJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct StatusErrorSummaryJson {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct AllStatusJson {
    profiles: Vec<StatusProfileJson>,
}

fn print_list_json(entries: &[Entry]) -> Result<(), String> {
    let profiles = entries
        .iter()
        .map(|entry| ListedProfile {
            id: entry.id.clone(),
            label: entry.label.clone(),
            email: entry.email.clone(),
            plan: entry.plan.clone(),
            is_current: entry.is_current,
            is_saved: entry.is_saved,
            is_api_key: entry.is_api_key,
            error: entry.error_summary.clone(),
        })
        .collect();
    let json = serde_json::to_string_pretty(&ListedProfiles { profiles })
        .map_err(|err| crate::msg1(PROFILE_ERR_SERIALIZE_INDEX, err))?;
    println!("{json}");
    Ok(())
}

fn status_error_summary_json(summary: String) -> StatusErrorSummaryJson {
    let summary = crate::sanitize_for_terminal(&summary);
    let Some((start, end, response)) = extract_embedded_json_object(&summary) else {
        return StatusErrorSummaryJson {
            message: summary,
            response: None,
        };
    };

    StatusErrorSummaryJson {
        message: strip_embedded_json_segment(&summary, start, end),
        response: Some(response),
    }
}

fn extract_embedded_json_object(summary: &str) -> Option<(usize, usize, serde_json::Value)> {
    for (start, ch) in summary.char_indices() {
        if ch != '{' {
            continue;
        }
        let Some(end) = find_json_object_end(summary, start) else {
            continue;
        };
        let candidate = &summary[start..end];
        let Ok(value) = serde_json::from_str::<serde_json::Value>(candidate) else {
            continue;
        };
        return Some((start, end, value));
    }
    None
}

fn find_json_object_end(text: &str, start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in text[start..].char_indices() {
        let idx = start + offset;
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    return Some(idx + ch.len_utf8());
                }
            }
            _ => {}
        }
    }

    None
}

fn strip_embedded_json_segment(text: &str, start: usize, end: usize) -> String {
    let left = text[..start].trim_end_matches([' ', ':']);
    let right = text[end..].trim_start_matches([',', ' ']);
    match (left.is_empty(), right.is_empty()) {
        (true, true) => String::new(),
        (true, false) => right.to_string(),
        (false, true) => left.to_string(),
        (false, false) => format!("{left}, {right}"),
    }
}

fn status_profile_json(entry: Entry) -> StatusProfileJson {
    let mut usage = entry.usage.map(|usage| StatusUsageJson {
        state: usage.state,
        buckets: usage.buckets,
        status_code: usage.status_code,
        summary: usage
            .summary
            .map(|summary| crate::sanitize_for_terminal(&summary)),
        detail: usage
            .detail
            .map(|detail| crate::sanitize_for_terminal(&detail)),
    });
    let mut top_level_summary = entry
        .error_summary
        .map(|error| crate::sanitize_for_terminal(&error));
    let mut error = None;
    if let Some(usage_json) = usage.as_mut()
        && usage_json.state == "error"
    {
        let status_code = usage_json.status_code.take();
        let detail = usage_json.detail.take();
        let usage_summary = usage_json.summary.take();
        let summary = top_level_summary.take().or(usage_summary);
        error = summary.map(|summary| StatusErrorJson {
            summary: status_error_summary_json(summary),
            status_code,
            detail,
        });
    }
    if error.is_none() {
        error = top_level_summary.map(|summary| StatusErrorJson {
            summary: status_error_summary_json(summary),
            status_code: None,
            detail: None,
        });
    }

    StatusProfileJson {
        id: entry.id,
        label: entry.label,
        email: entry.email,
        plan: entry.plan,
        is_current: entry.is_current,
        is_saved: entry.is_saved,
        is_api_key: entry.is_api_key,
        warnings: entry
            .warnings
            .into_iter()
            .map(|warning| crate::sanitize_for_terminal(&warning))
            .collect(),
        usage,
        error,
    }
}

fn print_current_status_json(current: Option<Entry>) -> Result<(), String> {
    let payload = current.map(status_profile_json);
    let json = serde_json::to_string_pretty(&payload)
        .map_err(|err| crate::msg1(PROFILE_ERR_SERIALIZE_INDEX, err))?;
    println!("{json}");
    Ok(())
}

fn print_all_status_json(profiles: Vec<Entry>) -> Result<(), String> {
    let payload = AllStatusJson {
        profiles: profiles.into_iter().map(status_profile_json).collect(),
    };
    let json = serde_json::to_string_pretty(&payload)
        .map_err(|err| crate::msg1(PROFILE_ERR_SERIALIZE_INDEX, err))?;
    println!("{json}");
    Ok(())
}

fn handle_inquire_result<T>(
    result: Result<T, inquire::error::InquireError>,
    context: &str,
) -> Result<T, String> {
    match result {
        Ok(value) => Ok(value),
        Err(err) if is_inquire_cancel(&err) => Err(CANCELLED_MESSAGE.to_string()),
        Err(err) => Err(crate::msg2(PROFILE_ERR_PROMPT_CONTEXT, context, err)),
    }
}

fn trim_label(label: &str) -> Result<&str, String> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return Err(PROFILE_ERR_LABEL_EMPTY.to_string());
    }
    Ok(trimmed)
}

fn is_profile_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    if ext != "json" {
        return false;
    }
    !matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("profiles.json" | "update.json")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{build_id_token, make_paths, set_env_guard};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn write_auth(
        path: &Path,
        account_id: &str,
        email: &str,
        plan: &str,
        access: &str,
        refresh: &str,
    ) {
        let id_token = build_id_token(email, plan);
        let value = serde_json::json!({
            "tokens": {
                "account_id": account_id,
                "id_token": id_token,
                "access_token": access,
                "refresh_token": refresh
            }
        });
        fs::write(path, serde_json::to_string(&value).unwrap()).unwrap();
    }

    fn write_profile(paths: &Paths, id: &str, account_id: &str, email: &str, plan: &str) {
        let id_token = build_id_token(email, plan);
        let value = serde_json::json!({
            "tokens": {
                "account_id": account_id,
                "id_token": id_token,
                "access_token": "acc",
                "refresh_token": "ref"
            }
        });
        let path = profile_path_for_id(&paths.profiles, id);
        fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
    }

    fn build_id_token_with_user(email: &str, plan: &str, user_id: &str) -> String {
        let header = serde_json::json!({
            "alg": "none",
            "typ": "JWT",
        });
        let auth = serde_json::json!({
            "chatgpt_plan_type": plan,
            "chatgpt_user_id": user_id,
        });
        let payload = serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": auth,
        });
        let header = URL_SAFE_NO_PAD.encode(serde_json::to_string(&header).unwrap());
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_string(&payload).unwrap());
        format!("{header}.{payload}.")
    }

    fn write_auth_with_user(
        path: &Path,
        account_id: &str,
        email: &str,
        plan: &str,
        user_id: &str,
        access: &str,
        refresh: &str,
    ) {
        let id_token = build_id_token_with_user(email, plan, user_id);
        let value = serde_json::json!({
            "tokens": {
                "account_id": account_id,
                "id_token": id_token,
                "access_token": access,
                "refresh_token": refresh
            }
        });
        fs::write(path, serde_json::to_string(&value).unwrap()).unwrap();
    }

    fn make_identity(principal: &str, workspace: &str, plan: &str) -> ProfileIdentityKey {
        ProfileIdentityKey {
            principal_id: principal.to_string(),
            workspace_or_org_id: workspace.to_string(),
            plan_type: plan.to_string(),
        }
    }

    fn make_tokens(account_id: &str, email: &str, plan: &str) -> Tokens {
        Tokens {
            account_id: Some(account_id.to_string()),
            id_token: Some(build_id_token(email, plan)),
            access_token: Some("acc".to_string()),
            refresh_token: Some("ref".to_string()),
        }
    }

    #[test]
    fn require_tty_with_variants() {
        assert!(require_tty_with(true, "load").is_ok());
        let err = require_tty_with(false, "load").unwrap_err();
        assert!(err.contains("requires a TTY"));
    }

    #[test]
    fn prompt_unsaved_load_with_variants() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        let err = prompt_unsaved_load_with(&paths, "reason", false, Ok(PROFILE_PROMPT_CANCEL))
            .unwrap_err();
        assert!(err.contains("not saved"));
        assert!(matches!(
            prompt_unsaved_load_with(&paths, "reason", true, Ok(PROFILE_PROMPT_SAVE_AND_CONTINUE))
                .unwrap(),
            LoadChoice::SaveAndContinue
        ));
        assert!(matches!(
            prompt_unsaved_load_with(
                &paths,
                "reason",
                true,
                Ok(PROFILE_PROMPT_CONTINUE_WITHOUT_SAVING)
            )
            .unwrap(),
            LoadChoice::ContinueWithoutSaving
        ));
        assert!(matches!(
            prompt_unsaved_load_with(&paths, "reason", true, Ok(PROFILE_PROMPT_CANCEL)).unwrap(),
            LoadChoice::Cancel
        ));
        let err = prompt_unsaved_load_with(
            &paths,
            "reason",
            true,
            Err(inquire::error::InquireError::OperationCanceled),
        )
        .unwrap();
        assert!(matches!(err, LoadChoice::Cancel));
    }

    #[test]
    fn confirm_delete_profiles_with_variants() {
        let err = confirm_delete_profiles_with(false, Ok(true)).unwrap_err();
        assert!(err.contains("requires confirmation"));
        assert!(confirm_delete_profiles_with(true, Ok(true)).unwrap());
        let err = confirm_delete_profiles_with(
            true,
            Err(inquire::error::InquireError::OperationCanceled),
        )
        .unwrap_err();
        assert_eq!(err, CANCELLED_MESSAGE);
    }

    #[test]
    fn label_helpers() {
        let mut labels = Labels::new();
        assign_label(&mut labels, "Team", "id").unwrap();
        assert_eq!(label_for_id(&labels, "id").unwrap(), "Team");
        assert_eq!(resolve_label_id(&labels, "Team").unwrap(), "id");
        remove_labels_for_id(&mut labels, "id");
        assert!(labels.is_empty());
        assert!(trim_label(" ").is_err());
    }

    #[test]
    fn ordered_profile_ids_prefers_current_then_label_then_email() {
        let mut labels = Labels::new();
        labels.insert("alpha".to_string(), "id-a".to_string());
        labels.insert("beta".to_string(), "id-b".to_string());
        labels.insert("zeta".to_string(), "id-z".to_string());

        let mut tokens = BTreeMap::new();
        tokens.insert(
            "id-z".to_string(),
            Ok(make_tokens("acct-z", "z@ex.com", "team")),
        );
        tokens.insert(
            "id-a".to_string(),
            Ok(make_tokens("acct-a", "a@ex.com", "team")),
        );
        tokens.insert(
            "id-u1".to_string(),
            Ok(make_tokens("acct-u1", "c@ex.com", "team")),
        );
        tokens.insert(
            "id-u2".to_string(),
            Ok(make_tokens("acct-u2", "b@ex.com", "team")),
        );
        tokens.insert(
            "id-b".to_string(),
            Ok(make_tokens("acct-b", "d@ex.com", "team")),
        );

        let snapshot = Snapshot {
            labels,
            tokens,
            index: ProfilesIndex::default(),
        };
        let ordered = ordered_profile_ids(&snapshot, Some("id-z"));
        assert_eq!(ordered, vec!["id-z", "id-a", "id-b", "id-u2", "id-u1"]);
    }

    #[test]
    fn usage_concurrency_defaults_and_clamps() {
        let _unset = set_env_guard(USAGE_CONCURRENCY_ENV, None);
        assert_eq!(usage_concurrency(), DEFAULT_USAGE_CONCURRENCY);

        let _zero = set_env_guard(USAGE_CONCURRENCY_ENV, Some("0"));
        assert_eq!(usage_concurrency(), DEFAULT_USAGE_CONCURRENCY);

        let _bad = set_env_guard(USAGE_CONCURRENCY_ENV, Some("oops"));
        assert_eq!(usage_concurrency(), DEFAULT_USAGE_CONCURRENCY);

        let _small = set_env_guard(USAGE_CONCURRENCY_ENV, Some("3"));
        assert_eq!(usage_concurrency(), 3);

        let _high = set_env_guard(USAGE_CONCURRENCY_ENV, Some("999"));
        assert_eq!(usage_concurrency(), MAX_USAGE_CONCURRENCY);
    }

    #[test]
    fn profiles_index_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        let mut index = ProfilesIndex::default();
        index.profiles.insert(
            "id".to_string(),
            ProfileIndexEntry {
                account_id: Some("acct".to_string()),
                email: Some("me@example.com".to_string()),
                plan: Some("Team".to_string()),
                label: Some("work".to_string()),
                is_api_key: false,
                principal_id: Some("principal-1".to_string()),
                workspace_or_org_id: Some("workspace-1".to_string()),
                plan_type_key: Some("team".to_string()),
            },
        );
        write_profiles_index(&paths, &index).unwrap();
        let read_back = read_profiles_index(&paths).unwrap();
        let entry = read_back.profiles.get("id").unwrap();
        assert_eq!(entry.account_id.as_deref(), Some("acct"));
        assert_eq!(entry.email.as_deref(), Some("me@example.com"));
        assert_eq!(entry.plan.as_deref(), Some("Team"));
        assert_eq!(entry.label.as_deref(), Some("work"));
        assert!(!entry.is_api_key);
        assert_eq!(entry.principal_id.as_deref(), Some("principal-1"));
        assert_eq!(entry.workspace_or_org_id.as_deref(), Some("workspace-1"));
        assert_eq!(entry.plan_type_key.as_deref(), Some("team"));
    }

    #[test]
    fn read_profiles_index_does_not_rewrite_when_legacy_strings_only_appear_in_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        let raw = serde_json::json!({
            "version": PROFILES_INDEX_VERSION,
            "profiles": {
                "id": {
                    "label": "default_profile_id update_cache active_profile_id last_used",
                    "is_api_key": false
                }
            }
        })
        .to_string();
        fs::write(&paths.profiles_index, &raw).unwrap();

        let _ = read_profiles_index(&paths).unwrap();
        let after = fs::read_to_string(&paths.profiles_index).unwrap();
        assert_eq!(after, raw);
    }

    #[test]
    fn profiles_index_prunes_missing_profiles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        let mut index = ProfilesIndex::default();
        index
            .profiles
            .insert("missing".to_string(), ProfileIndexEntry::default());
        prune_profiles_index(&mut index, &paths.profiles).unwrap();
        assert!(index.profiles.is_empty());
    }

    #[test]
    fn sanitize_helpers() {
        assert_eq!(sanitize_part("A B"), "a-b");
        assert_eq!(profile_base("", ""), "unknown-unknown");
        let identity = make_identity("principal", "workspace123", "team");
        assert_eq!(short_identity_suffix(&identity), "worksp");
        let unknown_workspace = make_identity("principal123", "unknown", "team");
        assert_eq!(short_identity_suffix(&unknown_workspace), "princi");
    }

    #[test]
    fn unique_id_conflicts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        write_profile(&paths, "base", "acct", "a@b.com", "pro");
        let id = unique_id(
            "base",
            &make_identity("acct", "acct", "pro"),
            &paths.profiles,
        );
        assert_eq!(id, "base");
        let id = unique_id(
            "base",
            &make_identity("other", "other", "pro"),
            &paths.profiles,
        );
        assert!(id.starts_with("base-"));
    }

    #[test]
    fn load_profile_tokens_map_handles_invalid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        let bad_path = paths.profiles.join("bad.json");
        write_profile(&paths, "valid", "acct", "a@b.com", "pro");
        fs::write(&bad_path, "not-json").unwrap();
        let index = serde_json::json!({
            "version": 1,
            "active_profile_id": null,
            "profiles": {
                "bad": {
                    "label": "bad",
                    "last_used": 1,
                    "added_at": 1
                }
            }
        });
        fs::write(
            &paths.profiles_index,
            serde_json::to_string(&index).unwrap(),
        )
        .unwrap();
        let map = load_profile_tokens_map(&paths).unwrap();
        assert!(map.contains_key("valid"));
        let bad = map.get("bad").expect("bad entry retained");
        assert!(bad.is_err());
        assert!(bad_path.is_file());

        let index_contents = fs::read_to_string(&paths.profiles_index).unwrap();
        assert!(index_contents.contains("\"bad\""));
    }

    #[test]
    fn load_profile_tokens_map_ignores_update_cache_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        fs::write(
            &paths.update_cache,
            serde_json::json!({
                "latest_version": "0.1.0",
                "last_checked_at": "2026-01-01T00:00:00Z"
            })
            .to_string(),
        )
        .unwrap();
        let map = load_profile_tokens_map(&paths).unwrap();
        assert!(map.is_empty());
        assert!(paths.update_cache.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn load_profile_tokens_map_remove_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        let bad_path = paths.profiles.join("bad.json");
        fs::write(&bad_path, "not-json").unwrap();
        let perms = fs::Permissions::from_mode(0o400);
        fs::set_permissions(&paths.profiles, perms).unwrap();
        let map = load_profile_tokens_map(&paths).unwrap();
        assert!(map.contains_key("bad"));
        fs::set_permissions(&paths.profiles, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(bad_path.is_file());
    }

    #[test]
    fn resolve_save_and_sync_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        write_profile(&paths, "one", "acct", "a@b.com", "pro");
        let tokens = read_tokens(&paths.profiles.join("one.json")).unwrap();
        let mut index = ProfilesIndex::default();
        let id = resolve_save_id(&paths, &mut index, &tokens).unwrap();
        assert!(!id.is_empty());
        let id = resolve_sync_id(&paths, &mut index, &tokens).unwrap();
        assert!(id.is_some());
    }

    #[test]
    fn rename_profile_id_errors_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        let mut index = ProfilesIndex::default();
        let err = rename_profile_id(
            &paths,
            &mut index,
            "missing",
            "base",
            &make_identity("acct", "acct", "pro"),
        )
        .unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn render_helpers() {
        let entry = Entry {
            id: Some("alpha@example.com-team".to_string()),
            label: Some("alpha".to_string()),
            email: Some("alpha@example.com".to_string()),
            plan: Some("team".to_string()),
            is_api_key: false,
            is_saved: true,
            display: "Display".to_string(),
            details: vec!["detail".to_string()],
            warnings: Vec::new(),
            usage: None,
            error_summary: None,
            always_show_details: true,
            is_current: false,
        };
        let ctx = ListCtx {
            base_url: None,
            base_url_error: None,
            now: chrono::Local::now(),
            show_usage: false,
            show_current_marker: false,
            show_id: true,
            use_color: false,
            profiles_dir: PathBuf::new(),
        };
        let lines = render_entries(&[entry], &ctx, true);
        assert!(!lines.is_empty());
        push_separator(&mut vec!["a".to_string()], true);
    }

    #[test]
    fn render_entries_preserves_ansi_display_in_color_mode() {
        colored::control::set_override(true);
        let entry = Entry {
            id: Some("alpha@example.com-team".to_string()),
            label: Some("alpha".to_string()),
            email: Some("alpha@example.com".to_string()),
            plan: Some("team".to_string()),
            is_api_key: false,
            is_saved: true,
            display: "\u{1b}[32malpha@example.com\u{1b}[0m".to_string(),
            details: Vec::new(),
            warnings: Vec::new(),
            usage: None,
            error_summary: None,
            always_show_details: false,
            is_current: false,
        };
        let ctx = ListCtx {
            base_url: None,
            base_url_error: None,
            now: chrono::Local::now(),
            show_usage: false,
            show_current_marker: false,
            show_id: false,
            use_color: true,
            profiles_dir: PathBuf::new(),
        };
        let lines = render_entries(&[entry], &ctx, true);
        colored::control::unset_override();

        assert!(!lines.is_empty());
        assert!(lines[0].contains("\u{1b}[32m"));
        assert_eq!(crate::ui::strip_ansi(&lines[0]), "alpha@example.com");
    }

    #[test]
    fn plain_error_lines_merges_unexpected_status_into_summary() {
        let lines = plain_error_lines(
            "deactivated_workspace\nunexpected status 402 Payment Required\nURL: http://localhost/backend-api/wham/usage",
            false,
        );

        assert_eq!(
            lines[0],
            "Error: deactivated_workspace (unexpected status 402 Payment Required)"
        );
        assert_eq!(lines[1], " URL: http://localhost/backend-api/wham/usage");
    }

    #[test]
    fn render_entries_status_all_has_extra_gap_between_profiles() {
        let entries = vec![
            Entry {
                id: Some("one".to_string()),
                label: None,
                email: Some("one@example.com".to_string()),
                plan: Some("team".to_string()),
                is_api_key: false,
                is_saved: true,
                display: "One".to_string(),
                details: vec!["5 hour: 10% left".to_string()],
                warnings: Vec::new(),
                usage: None,
                error_summary: None,
                always_show_details: true,
                is_current: false,
            },
            Entry {
                id: Some("two".to_string()),
                label: None,
                email: Some("two@example.com".to_string()),
                plan: Some("team".to_string()),
                is_api_key: false,
                is_saved: true,
                display: "Two".to_string(),
                details: vec!["5 hour: 20% left".to_string()],
                warnings: Vec::new(),
                usage: None,
                error_summary: None,
                always_show_details: true,
                is_current: false,
            },
        ];
        let ctx = ListCtx {
            base_url: None,
            base_url_error: None,
            now: chrono::Local::now(),
            show_usage: true,
            show_current_marker: false,
            show_id: false,
            use_color: false,
            profiles_dir: PathBuf::new(),
        };
        let lines = render_entries(&entries, &ctx, true);
        let first_profile_last_line = 2;
        assert_eq!(lines[first_profile_last_line + 1], "");
        assert_eq!(lines[first_profile_last_line + 2], "");
    }

    #[test]
    fn strip_ansi_sequences_removes_color_codes() {
        assert_eq!(crate::ui::strip_ansi("\u{1b}[31mtext\u{1b}[0m"), "text");
    }

    #[test]
    fn handle_inquire_result_variants() {
        let ok: Result<i32, inquire::error::InquireError> = Ok(1);
        assert_eq!(handle_inquire_result(ok, "selection").unwrap(), 1);
        let err: Result<(), inquire::error::InquireError> =
            Err(inquire::error::InquireError::OperationCanceled);
        let err = handle_inquire_result(err, "selection").unwrap_err();
        assert_eq!(err, CANCELLED_MESSAGE);
    }

    #[test]
    fn is_http_401_message_variants() {
        assert!(is_http_401_message(&crate::msg2(
            crate::UI_ERROR_TWO_LINE,
            crate::AUTH_REFRESH_401_TITLE,
            crate::AUTH_RELOGIN_AND_SAVE
        )));
        assert!(is_http_401_message("Error: Unauthorized (401)"));
        assert!(!is_http_401_message(&crate::msg1(
            "Error: {}",
            crate::USAGE_UNAVAILABLE_402_TITLE
        )));
    }

    #[test]
    fn sync_and_status_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        write_auth(&paths.auth, "acct", "a@b.com", "pro", "acc", "ref");
        crate::ensure_paths(&paths).unwrap();
        save_profile(&paths, Some("team".to_string()), false).unwrap();
        list_profiles(&paths, false, false).unwrap();
        status_profiles(&paths, false, None, None, false).unwrap();
        status_profiles(&paths, true, None, None, false).unwrap();
    }

    #[test]
    fn delete_profile_by_label() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        write_auth(&paths.auth, "acct", "a@b.com", "pro", "acc", "ref");
        crate::ensure_paths(&paths).unwrap();
        save_profile(&paths, Some("team".to_string()), false).unwrap();
        delete_profile(&paths, true, Some("team".to_string()), vec![], false).unwrap();
    }

    #[test]
    fn composite_identity_repeated_save_dedupes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        write_auth_with_user(
            &paths.auth,
            "acct-1",
            "same@example.com",
            "pro",
            "user-1",
            "acc",
            "ref",
        );
        crate::ensure_paths(&paths).unwrap();

        save_profile(&paths, None, false).unwrap();
        save_profile(&paths, None, false).unwrap();

        let ids = collect_profile_ids(&paths.profiles).unwrap();
        assert_eq!(ids.len(), 1);
        assert!(ids.contains("same@example.com-pro"));
    }

    #[test]
    fn composite_identity_keeps_team_and_pro_separate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        crate::ensure_paths(&paths).unwrap();

        write_auth_with_user(
            &paths.auth,
            "acct-1",
            "same@example.com",
            "pro",
            "user-1",
            "acc",
            "ref",
        );
        save_profile(&paths, None, false).unwrap();

        write_auth_with_user(
            &paths.auth,
            "acct-1",
            "same@example.com",
            "team",
            "user-1",
            "acc",
            "ref",
        );
        save_profile(&paths, None, false).unwrap();

        let ids = collect_profile_ids(&paths.profiles).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("same@example.com-pro"));
        assert!(ids.contains("same@example.com-team"));
    }

    #[test]
    fn composite_identity_separates_users_in_same_workspace_plan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        crate::ensure_paths(&paths).unwrap();

        write_auth_with_user(
            &paths.auth,
            "acct-1",
            "same@example.com",
            "pro",
            "user-1",
            "acc",
            "ref",
        );
        save_profile(&paths, None, false).unwrap();

        write_auth_with_user(
            &paths.auth,
            "acct-1",
            "same@example.com",
            "pro",
            "user-2",
            "acc",
            "ref",
        );
        save_profile(&paths, None, false).unwrap();

        let ids = collect_profile_ids(&paths.profiles).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("same@example.com-pro"));
        assert!(
            ids.iter()
                .any(|id| id.starts_with("same@example.com-pro-acct"))
        );
    }
}
