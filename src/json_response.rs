use serde::{Deserialize, Serialize};

/// Unified JSON response for mutating commands (`save`, `load`, `delete`, `label`, `default`, `export`, `import`).
///
/// On success `profile` is present; on error callers return `Err(...)` which the CLI runner
/// serialises via the standard `eprintln!` + `exit(1)` path. The `--json` flag only changes
/// the success output — errors remain text on stderr so scripts can distinguish them via exit
/// code without needing to parse stderr JSON.
#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResultJson {
    /// The command name (e.g. "save", "load", "delete", "export", "import").
    pub command: String,
    /// `true` when the operation succeeded.
    pub success: bool,
    /// Resulting data — profile details for profile-mutating commands, summary object for
    /// export/import. Always present when `success` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<serde_json::Value>,
}

impl CommandResultJson {
    /// Construct a success response carrying `data` as the `profile` field.
    pub fn success(command: &str, data: serde_json::Value) -> Self {
        Self {
            command: command.to_string(),
            success: true,
            profile: Some(data),
        }
    }

    /// Serialize `self` to pretty-printed JSON and print it to stdout.
    pub fn print(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self)
            .expect("CommandResultJson contains only infallible JSON values");
        println!("{json}");
        Ok(())
    }
}
