use clap::{ArgAction, Args, Command, CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

use crate::command_name;

#[derive(Parser)]
#[command(author, version, about, color = clap::ColorChoice::Never)]
pub struct Cli {
    /// Disable styling and separators
    #[arg(long, global = true)]
    pub plain: bool,
    /// Print machine-readable JSON success output
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Save the current auth.json as a profile
    Save {
        /// Optional label for the profile (must be unique)
        #[arg(value_name = "label")]
        #[arg(long)]
        label: Option<String>,
    },
    /// Load a saved profile
    Load {
        /// Load the profile matching this label
        #[arg(value_name = "label")]
        #[arg(long, conflicts_with = "id")]
        label: Option<String>,
        /// Load the profile matching this exact id
        #[arg(value_name = "profile-id")]
        #[arg(long, conflicts_with = "label")]
        id: Option<String>,
        /// Continue without saving the current unsaved profile first
        #[arg(long)]
        force: bool,
        /// Show status for the loaded profile after a successful load
        #[arg(long)]
        with_status: bool,
    },
    /// List saved profiles
    List {
        /// Show profile ids in human-readable output (JSON already includes them)
        #[arg(long)]
        show_id: bool,
    },
    /// Export saved profiles for backup or transfer
    Export {
        /// Export only the profile matching this label
        #[arg(value_name = "label")]
        #[arg(long, conflicts_with = "id")]
        label: Option<String>,
        /// Export only the profile(s) matching these exact ids (repeatable)
        #[arg(value_name = "profile-id")]
        #[arg(long, conflicts_with = "label", action = ArgAction::Append)]
        id: Vec<String>,
        /// Write the export bundle to this new file
        #[arg(long, value_name = "file")]
        output: PathBuf,
    },
    /// Import saved profiles from an export bundle
    Import {
        /// Read the export bundle from this file (fails on id or label conflicts)
        #[arg(long, value_name = "file")]
        input: PathBuf,
    },
    /// Run local diagnostics
    Doctor {
        /// Apply safe repairs for profile storage metadata
        #[arg(long)]
        fix: bool,
    },
    /// Manage saved profile labels
    Label {
        #[command(subcommand)]
        command: LabelCommands,
    },
    /// Show usage details for the current or saved profiles
    Status {
        /// Show usage for all saved profiles
        #[arg(long, conflicts_with = "label", conflicts_with = "id")]
        all: bool,
        /// Show usage for the saved profile matching this label
        #[arg(
            long,
            value_name = "label",
            conflicts_with = "id",
            conflicts_with = "all"
        )]
        label: Option<String>,
        /// Show usage for the saved profile matching this exact id
        #[arg(
            long,
            value_name = "profile-id",
            conflicts_with = "label",
            conflicts_with = "all"
        )]
        id: Option<String>,
    },
    /// Delete saved profiles
    Delete {
        /// Skip delete confirmation
        #[arg(long)]
        yes: bool,
        /// Delete the profile matching this label
        #[arg(value_name = "label")]
        #[arg(long, conflicts_with = "id")]
        label: Option<String>,
        /// Delete the profile(s) matching these exact ids (repeatable)
        #[arg(value_name = "profile-id")]
        #[arg(long, conflicts_with = "label", action = ArgAction::Append)]
        id: Vec<String>,
    },
}

#[derive(Args)]
#[group(multiple = false)]
pub struct SavedProfileSelector {
    /// Select the profile matching this existing label
    #[arg(value_name = "label")]
    #[arg(long)]
    pub label: Option<String>,
    /// Select the profile matching this exact id
    #[arg(value_name = "profile-id")]
    #[arg(long)]
    pub id: Option<String>,
}

#[derive(Subcommand)]
pub enum LabelCommands {
    /// Set or replace a label on a saved profile
    Set {
        #[command(flatten)]
        selector: SavedProfileSelector,
        /// New label to assign
        #[arg(long, value_name = "label")]
        to: String,
    },
    /// Clear the label on a saved profile
    Clear {
        #[command(flatten)]
        selector: SavedProfileSelector,
    },
    /// Rename an existing label
    Rename {
        /// Existing label to rename
        #[arg(value_name = "label")]
        #[arg(long)]
        label: String,
        /// New label to assign
        #[arg(long, value_name = "label")]
        to: String,
    },
}

pub fn command_with_examples() -> Command {
    let name = command_name();
    let mut cmd = Cli::command();
    cmd.set_bin_name(name);
    cmd = cmd.mut_subcommand("label", |label| {
        label
            .mut_subcommand("set", |set| set.override_usage(label_set_usage(name)))
            .mut_subcommand("clear", |clear| {
                clear.override_usage(label_clear_usage(name))
            })
    });
    cmd = cmd.after_help(examples_root(name));
    cmd
}

pub fn label_set_usage(name: &str) -> String {
    format!("{name} label set [OPTIONS] --to <label> (--label <label> | --id <profile-id>)")
}

pub fn label_clear_usage(name: &str) -> String {
    format!("{name} label clear [OPTIONS] (--label <label> | --id <profile-id>)")
}

fn examples_root(name: &str) -> String {
    format!(
        "Examples:\n  {name} save --label work\n  {name} load --label work\n  {name} list --json\n  {name} status --all --json\n  {name} export --output profiles-export.json\n  {name} import --input profiles-export.json\n  {name} delete --label work --yes\n\nUse `--json` for machine-readable success output. Run `{name} help <command>` for command-specific options."
    )
}

#[cfg(test)]
mod tests {
    use super::examples_root;

    #[test]
    fn examples_root_uses_clear_professional_headings() {
        let text = examples_root("codex-profiles");
        assert!(text.contains("Examples:"));
        assert!(text.contains("Use `--json` for machine-readable success output."));
        assert!(!text.contains("Common options:"));
        assert!(!text.contains("Machine-readable output:"));
    }
}
