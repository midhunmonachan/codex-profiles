use clap::{FromArgMatches, error::ErrorKind};
use std::io::{self, Write};
use std::process::Command as ProcessCommand;

use crate::cli::{Cli, Commands, command_with_examples};

pub fn run_cli() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if let Err(message) = run_cli_with_args(args) {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn run_cli_with_args(args: Vec<std::ffi::OsString>) -> Result<(), String> {
    if args.len() == 1 {
        print_version_header();
        let mut cmd = command_with_examples();
        let _ = cmd.print_help();
        println!();
        return Ok(());
    }
    let cmd = command_with_examples();
    let matches = match cmd.clone().try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(err) => {
            if err.kind() == ErrorKind::DisplayHelp {
                print_version_header();
                let _ = err.print();
                println!();
                return Ok(());
            }
            if err.kind() == ErrorKind::DisplayVersion {
                return write_version_error(&err);
            }
            return Err(err.to_string());
        }
    };
    let cli = Cli::from_arg_matches(&matches)
        .expect("validated clap matches must convert to the derived CLI type");
    set_plain(cli.plain);
    if let Err(message) = run(cli) {
        if message == CANCELLED_MESSAGE {
            let message = format_cancel(use_color_stdout());
            print_output_block(&message);
            return Ok(());
        }
        return Err(message);
    }
    Ok(())
}

fn write_version_error(err: &clap::error::Error) -> Result<(), String> {
    let mut stdout = io::stdout().lock();
    write_version_error_to(err, &mut stdout)
}

fn write_version_error_to(err: &clap::error::Error, writer: &mut dyn Write) -> Result<(), String> {
    write!(writer, "{}", err.render())
        .and_then(|_| writer.flush())
        .map_err(|error| format!("Could not write version response: {error}"))
}

fn print_version_header() {
    let name = package_command_name();
    println!("{name} {}", env!("CARGO_PKG_VERSION"));
    println!();
}

fn run(cli: Cli) -> Result<(), String> {
    let paths = resolve_paths()?;
    let json = cli.json;
    let is_doctor = matches!(&cli.command, Commands::Doctor { .. });
    let update_outcome = if is_doctor {
        None
    } else {
        ensure_paths(&paths)?;
        let check_for_update_on_startup = std::env::var_os("CODEX_PROFILES_SKIP_UPDATE").is_none();
        let update_config = UpdateConfig {
            codex_home: paths.codex.clone(),
            check_for_update_on_startup,
        };
        Some(run_update_prompt_if_needed(&update_config)?)
    };
    run_with_update_outcome(cli, paths, json, update_outcome)
}

fn run_with_update_outcome(
    cli: Cli,
    paths: Paths,
    json: bool,
    update_outcome: Option<UpdatePromptOutcome>,
) -> Result<(), String> {
    if let Some(UpdatePromptOutcome::RunUpdate(action)) = update_outcome {
        return run_update_action(action);
    }

    match cli.command {
        Commands::Save { label } => save_profile(&paths, label, json),
        Commands::Load {
            label,
            id,
            force,
            with_status,
        } => load_profile(&paths, label, id, force, with_status, json),
        Commands::List { show_id } => list_profiles(&paths, json, show_id),
        Commands::Export { label, id, output } => export_profiles(&paths, label, id, output, json),
        Commands::Import { input } => import_profiles(&paths, input, json),
        Commands::Doctor { fix } => doctor(&paths, fix, json),
        Commands::Label { command } => match command {
            crate::cli::LabelCommands::Set { selector, to } => {
                let (label, id) = require_saved_profile_selector(
                    selector,
                    crate::cli::label_set_usage(command_name()),
                )?;
                set_profile_label(&paths, label, id, to, json)
            }
            crate::cli::LabelCommands::Clear { selector } => {
                let (label, id) = require_saved_profile_selector(
                    selector,
                    crate::cli::label_clear_usage(command_name()),
                )?;
                clear_profile_label(&paths, label, id, json)
            }
            crate::cli::LabelCommands::Rename { label, to } => {
                rename_profile_label(&paths, label, to, json)
            }
        },
        Commands::Status {
            all,
            compact,
            label,
            id,
        } => status_profiles(&paths, all, compact, label, id, json),
        Commands::Delete { yes, label, id } => delete_profile(&paths, yes, label, id, json),
    }
}

fn require_saved_profile_selector(
    selector: crate::cli::SavedProfileSelector,
    usage: String,
) -> Result<(Option<String>, Option<String>), String> {
    if selector.label.is_none() && selector.id.is_none() {
        return Err(format!(
            "error: exactly one of `--label <label>` or `--id <profile-id>` is required.\n\nUsage: {usage}\n\nFor more information, try '--help'."
        ));
    }
    Ok((selector.label, selector.id))
}

fn run_update_action(action: UpdateAction) -> Result<(), String> {
    let (command, args) = action.command_args();
    let status = ProcessCommand::new(command)
        .args(args)
        .status()
        .map_err(|err| crate::msg1(crate::CMD_ERR_UPDATE_RUN, err))?;
    if status.success() {
        Ok(())
    } else {
        Err(crate::msg1(
            crate::CMD_ERR_UPDATE_FAILED,
            action.command_str(),
        ))
    }
}
mod auth;
mod cli;
mod common;
mod doctor;
mod json_response;
mod messages;
mod profiles;
#[cfg(test)]
mod test_utils;
mod ui;
mod updates;
mod usage;

pub(crate) use auth::*;
pub(crate) use common::*;
pub(crate) use doctor::*;
pub(crate) use messages::*;
pub(crate) use profiles::*;
pub(crate) use ui::*;
pub(crate) use updates::*;
pub(crate) use usage::*;

pub use auth::{AuthFile, Tokens, extract_email_and_plan};
pub use updates::{
    InstallSource, detect_install_source_inner, extract_version_from_cask,
    extract_version_from_latest_tag, is_newer,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_paths, set_env_guard};
    use std::ffi::OsString;
    use std::fs;

    #[test]
    fn run_cli_with_args_help() {
        let args = vec![OsString::from("codex-profiles")];
        run_cli_with_args(args).unwrap();
    }

    #[test]
    fn run_cli_with_args_display_help() {
        let args = vec![OsString::from("codex-profiles"), OsString::from("--help")];
        run_cli_with_args(args).unwrap();
    }

    #[test]
    fn run_cli_with_args_display_version() {
        let args = vec![
            OsString::from("codex-profiles"),
            OsString::from("--version"),
        ];
        run_cli_with_args(args).unwrap();
    }

    #[test]
    fn display_version_write_errors_are_returned() {
        let err = command_with_examples()
            .try_get_matches_from(["codex-profiles", "--version"])
            .unwrap_err();
        let mut writer = FailingWriter {
            fail_write: true,
            fail_flush: false,
            output: Vec::new(),
        };
        let message = write_version_error_to(&err, &mut writer).unwrap_err();
        assert!(message.contains("Could not write version response"));
    }

    #[test]
    fn display_version_flush_errors_are_returned() {
        let err = command_with_examples()
            .try_get_matches_from(["codex-profiles", "--version"])
            .unwrap_err();
        let mut writer = FailingWriter {
            fail_write: false,
            fail_flush: true,
            output: Vec::new(),
        };
        let message = write_version_error_to(&err, &mut writer).unwrap_err();
        assert!(message.contains("Could not write version response"));
    }

    #[test]
    fn display_version_output_is_rendered_before_flush() {
        let err = command_with_examples()
            .try_get_matches_from(["codex-profiles", "--version"])
            .unwrap_err();
        let expected = err.render().to_string();
        let mut writer = FailingWriter {
            fail_write: false,
            fail_flush: false,
            output: Vec::new(),
        };
        write_version_error_to(&err, &mut writer).unwrap();
        assert_eq!(String::from_utf8(writer.output).unwrap(), expected);
    }

    struct FailingWriter {
        fail_write: bool,
        fail_flush: bool,
        output: Vec<u8>,
    }

    impl Write for FailingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                return Err(io::Error::other("synthetic writer failure"));
            }
            self.output.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                return Err(io::Error::other("synthetic flush failure"));
            }
            Ok(())
        }
    }

    #[test]
    fn run_cli_with_args_errors() {
        let args = vec![OsString::from("codex-profiles"), OsString::from("nope")];
        let err = run_cli_with_args(args).unwrap_err();
        assert!(err.contains("error"));
    }

    #[cfg(unix)]
    #[test]
    fn run_update_action_paths() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = crate::test_utils::ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("npm");
        fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        {
            let _env = set_env_guard("PATH", Some(&path));
            run_update_action(UpdateAction::NpmGlobalLatest).unwrap();
            run_with_update_outcome(
                Cli {
                    plain: true,
                    json: false,
                    command: Commands::List { show_id: false },
                },
                make_paths(dir.path()),
                false,
                Some(UpdatePromptOutcome::RunUpdate(
                    UpdateAction::NpmGlobalLatest,
                )),
            )
            .unwrap();
        }
        fs::write(&bin, "#!/bin/sh\nexit 1\n").unwrap();
        {
            let _env = set_env_guard("PATH", Some(&path));
            let err = run_update_action(UpdateAction::NpmGlobalLatest).unwrap_err();
            assert!(err.contains("Update command failed"));
        }
        {
            let _env = set_env_guard("PATH", Some(""));
            let err = run_update_action(UpdateAction::NpmGlobalLatest).unwrap_err();
            assert!(err.contains("Could not run update command"));
        }
    }

    #[test]
    fn run_cli_list_command() {
        let _guard = crate::test_utils::ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        let home = dir.path().to_string_lossy().into_owned();
        let _home = set_env_guard("CODEX_PROFILES_HOME", Some(&home));
        let _codex_home = set_env_guard("CODEX_HOME", None);
        let _skip = set_env_guard("CODEX_PROFILES_SKIP_UPDATE", Some("1"));
        let cli = Cli {
            plain: true,
            json: false,
            command: Commands::List { show_id: false },
        };
        run(cli).unwrap();
    }

    #[test]
    fn run_dispatches_every_normal_command_route() {
        let _guard = crate::test_utils::ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        ensure_paths(&paths).unwrap();
        fs::write(&paths.auth, r#"{"OPENAI_API_KEY":"sk-dispatch-test"}"#).unwrap();

        let dispatch = |command| {
            run_with_update_outcome(
                Cli {
                    plain: true,
                    json: true,
                    command,
                },
                make_paths(dir.path()),
                true,
                None,
            )
        };

        dispatch(Commands::Save {
            label: Some("work".to_string()),
        })
        .unwrap();
        dispatch(Commands::List { show_id: true }).unwrap();
        dispatch(Commands::Status {
            all: false,
            compact: false,
            label: None,
            id: None,
        })
        .unwrap();

        let export_path = dir.path().join("profiles.json");
        dispatch(Commands::Export {
            label: Some("work".to_string()),
            id: Vec::new(),
            output: export_path.clone(),
        })
        .unwrap();

        dispatch(Commands::Load {
            label: Some("work".to_string()),
            id: None,
            force: true,
            with_status: false,
        })
        .unwrap();

        let index: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&paths.profiles_index).unwrap()).unwrap();
        let id = index["profiles"]
            .as_object()
            .and_then(|profiles| profiles.keys().next())
            .expect("saved profile id")
            .to_string();
        dispatch(Commands::Label {
            command: crate::cli::LabelCommands::Set {
                selector: crate::cli::SavedProfileSelector {
                    label: None,
                    id: Some(id.clone()),
                },
                to: "team".to_string(),
            },
        })
        .unwrap();
        dispatch(Commands::Label {
            command: crate::cli::LabelCommands::Rename {
                label: "team".to_string(),
                to: "renamed".to_string(),
            },
        })
        .unwrap();
        dispatch(Commands::Label {
            command: crate::cli::LabelCommands::Clear {
                selector: crate::cli::SavedProfileSelector {
                    label: None,
                    id: Some(id.clone()),
                },
            },
        })
        .unwrap();

        let missing_set_selector = dispatch(Commands::Label {
            command: crate::cli::LabelCommands::Set {
                selector: crate::cli::SavedProfileSelector {
                    label: None,
                    id: None,
                },
                to: "team".to_string(),
            },
        })
        .unwrap_err();
        assert!(
            missing_set_selector
                .contains("exactly one of `--label <label>` or `--id <profile-id>` is required")
        );
        assert!(missing_set_selector.contains("Usage:"));

        let missing_clear_selector = dispatch(Commands::Label {
            command: crate::cli::LabelCommands::Clear {
                selector: crate::cli::SavedProfileSelector {
                    label: None,
                    id: None,
                },
            },
        })
        .unwrap_err();
        assert!(
            missing_clear_selector
                .contains("exactly one of `--label <label>` or `--id <profile-id>` is required")
        );
        assert!(missing_clear_selector.contains("Usage:"));

        dispatch(Commands::Doctor { fix: false }).unwrap();

        let imported_dir = tempfile::tempdir().expect("import tempdir");
        let imported_paths = make_paths(imported_dir.path());
        ensure_paths(&imported_paths).unwrap();
        run_with_update_outcome(
            Cli {
                plain: true,
                json: true,
                command: Commands::Import { input: export_path },
            },
            imported_paths,
            true,
            None,
        )
        .unwrap();

        dispatch(Commands::Delete {
            yes: true,
            label: None,
            id: vec![id],
        })
        .unwrap();
    }
}
