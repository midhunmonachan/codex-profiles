use colored::Colorize;
use inquire::ui::{Color, RenderConfig, StyleSheet, Styled};
use std::sync::atomic::{AtomicBool, Ordering};
use supports_color::Stream;

use crate::has_auth;
use crate::{
    CANCELLED_MESSAGE, UI_ERROR_PREFIX, UI_HINT_ADD_LABEL_LATER, UI_HINT_FORCE_LOAD,
    UI_HINT_LIST_PROFILES, UI_HINT_LOGIN_AND_SAVE, UI_HINT_LOGIN_SAVE_BEFORE_LOADING,
    UI_HINT_SAVE_BEFORE_LOADING, UI_HINT_SAVE_PROFILE, UI_INFO_PREFIX, UI_NO_SAVED_PROFILES,
    UI_NORMALIZED_AUTH_INCOMPLETE, UI_NORMALIZED_AUTH_INVALID, UI_NORMALIZED_NOT_LOGGED_IN,
    UI_UNKNOWN_PROFILE, UI_WARNING_PREFIX, UI_WARNING_UNSAVED_PROFILE,
};
use crate::{Paths, command_name};

static PLAIN: AtomicBool = AtomicBool::new(false);

pub fn set_plain(value: bool) {
    PLAIN.store(value, Ordering::Relaxed);
}

pub fn is_plain() -> bool {
    PLAIN.load(Ordering::Relaxed)
}

pub fn use_color_stdout() -> bool {
    supports_color(Stream::Stdout)
}

pub fn use_color_stderr() -> bool {
    supports_color(Stream::Stderr)
}

fn supports_color(stream: Stream) -> bool {
    if is_plain() {
        return false;
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    supports_color::on(stream).is_some()
}

pub fn style_text<F>(text: &str, use_color: bool, style: F) -> String
where
    F: FnOnce(colored::ColoredString) -> colored::ColoredString,
{
    if use_color && !is_plain() {
        style(text.normal()).to_string()
    } else {
        text.to_string()
    }
}

pub fn format_cmd(command: &str, use_color: bool) -> String {
    let text = format!("`{command}`");
    style_text(&text, use_color, |text| text.yellow().bold())
}

pub fn format_action(message: &str, use_color: bool) -> String {
    let text = format!("✅ {message}");
    style_text(&text, use_color, |text| text.green().bold())
}

pub fn format_warning(message: &str, use_color: bool) -> String {
    let prefix = UI_WARNING_PREFIX;
    let mut lines = message.lines();
    let first = lines.next().unwrap_or_default();
    let mut text = format!("{prefix}{first}");
    let indent = " ".repeat(prefix.len());
    for line in lines {
        text.push('\n');
        text.push_str(&indent);
        text.push_str(line);
    }
    style_text(&text, use_color, |text| text.yellow().dimmed().italic())
}

pub fn format_cancel(use_color: bool) -> String {
    style_text(CANCELLED_MESSAGE, use_color, |text| text.dimmed().italic())
}

pub fn format_hint(message: &str, use_color: bool) -> String {
    if !use_color {
        crate::msg1(UI_INFO_PREFIX, message)
    } else {
        let message = format!("\n\n{message}");
        style_text(&message, use_color, |text| text.italic())
    }
}

pub fn format_no_profiles(paths: &Paths, use_color: bool) -> String {
    let hint = format_save_hint(
        paths,
        use_color,
        UI_HINT_SAVE_PROFILE,
        UI_HINT_LOGIN_AND_SAVE,
    );
    crate::msg1(UI_NO_SAVED_PROFILES, hint)
}

pub fn format_save_before_load_or_force(paths: &Paths, use_color: bool) -> String {
    let base = format_save_hint_message(
        paths,
        use_color,
        UI_HINT_SAVE_BEFORE_LOADING,
        UI_HINT_LOGIN_SAVE_BEFORE_LOADING,
    );
    let force = format_command("load --force", use_color);
    let message = format!("{base}\n{}", UI_HINT_FORCE_LOAD.replace("{force}", &force));
    format_hint(&message, use_color)
}

pub fn format_label_later_hint(id: &str, use_color: bool) -> String {
    let label_cmd = format_command(&format!("label set --id {id} --to work"), use_color);
    format_hint(
        &UI_HINT_ADD_LABEL_LATER.replace("{label_cmd}", &label_cmd),
        use_color,
    )
}

pub fn format_unsaved_warning(use_color: bool) -> Vec<String> {
    let warning = UI_WARNING_UNSAVED_PROFILE;
    let save_line = UI_HINT_SAVE_PROFILE.replace("{save}", &format_command("save", use_color));
    if !use_color {
        return vec![warning.to_string(), save_line];
    }
    vec![
        style_text(warning, use_color, |text| text.yellow().dimmed().italic()),
        style_text(&save_line, use_color, |text| text.dimmed().italic()),
    ]
}

pub fn format_list_hint(use_color: bool) -> String {
    let list = format_command("list", use_color);
    format_hint(&UI_HINT_LIST_PROFILES.replace("{list}", &list), use_color)
}

pub fn normalize_error(message: &str) -> String {
    let message = message.strip_prefix(UI_ERROR_PREFIX).unwrap_or(message);
    let message = message.trim_start();
    let message_lower = message.to_ascii_lowercase();
    if message_lower.contains("codex login")
        && !message.contains("(401)")
        && !message_lower.contains("unauthorized")
    {
        if message_lower.contains("not found") {
            return UI_NORMALIZED_NOT_LOGGED_IN.to_string();
        }
        if message_lower.contains("invalid json") {
            return UI_NORMALIZED_AUTH_INVALID.to_string();
        }
        return UI_NORMALIZED_AUTH_INCOMPLETE.to_string();
    }
    message.to_string()
}

pub fn format_error(message: &str) -> String {
    let normalized = normalize_error(message);
    let use_color = use_color_stdout();
    let prefix = if use_color {
        UI_ERROR_PREFIX.red().bold().to_string()
    } else {
        UI_ERROR_PREFIX.to_string()
    };
    let indent = " ".repeat(prefix.chars().count());
    let mut lines = normalized.lines();
    let first = lines.next().unwrap_or_default();
    let mut text = format!("{prefix}{first}");
    for line in lines {
        text.push('\n');
        text.push_str(&indent);
        text.push_str(&style_text(line, use_color, |text| text.dimmed().italic()));
    }
    text
}

pub fn format_profile_display(
    email: Option<String>,
    plan: Option<String>,
    label: Option<String>,
    is_current: bool,
    use_color: bool,
) -> String {
    let label = label.map(|value| crate::sanitize_for_terminal(&value));
    let email = email.map(|value| crate::sanitize_for_terminal(&value));
    let plan = plan.map(|value| crate::sanitize_for_terminal(&value));
    let label = label.as_deref();
    if email
        .as_deref()
        .map(|value| value.eq_ignore_ascii_case("Key"))
        .unwrap_or(false)
        && plan
            .as_deref()
            .map(|value| value.eq_ignore_ascii_case("Key"))
            .unwrap_or(false)
    {
        let badge = format_plan_badge("Key", is_current, use_color);
        let label_suffix = format_label(label, use_color);
        return format!("{badge}{label_suffix}");
    }
    let label_suffix = format_label(label, use_color);
    match email {
        Some(email) => {
            let plan = plan.unwrap_or_else(|| "Unknown".to_string());
            let badge = format_plan_badge(&plan, is_current, use_color);
            if use_color {
                let email_badge = format_email_badge(&email, is_current);
                format!("{badge}{email_badge}{label_suffix}")
            } else {
                format!("{badge} {email}{label_suffix}")
            }
        }
        None => crate::msg1(UI_UNKNOWN_PROFILE, label_suffix),
    }
}

pub fn format_entry_header(display: &str, use_color: bool) -> String {
    if use_color {
        display.bold().to_string()
    } else {
        crate::sanitize_for_terminal(display)
    }
}

fn format_plan_badge(plan: &str, _is_current: bool, use_color: bool) -> String {
    let plan_upper = plan.to_uppercase();
    let text = format!(" {} ", plan_upper);
    if use_color {
        text.white().on_bright_black().to_string()
    } else {
        format!("[{plan_upper}]")
    }
}

fn format_label(label: Option<&str>, use_color: bool) -> String {
    match label {
        Some(value) if use_color => format!(" {value} ").black().on_white().dimmed().to_string(),
        Some(value) => format!(" ({value})"),
        None => String::new(),
    }
}

fn format_email_badge(email: &str, _is_current: bool) -> String {
    format!(" {email} ").bright_black().on_white().to_string()
}

pub fn inquire_select_render_config() -> RenderConfig<'static> {
    let mut config = if use_color_stderr() {
        let mut config = RenderConfig::default_colored();
        config.help_message = StyleSheet::new().with_fg(Color::DarkGrey);
        config
    } else {
        RenderConfig::empty()
    };
    config.prompt_prefix = Styled::new("");
    config.answered_prompt_prefix = Styled::new("");
    config
}

pub fn is_inquire_cancel(err: &inquire::error::InquireError) -> bool {
    matches!(
        err,
        inquire::error::InquireError::OperationCanceled
            | inquire::error::InquireError::OperationInterrupted
    )
}

const OUTPUT_INDENT: &str = " ";

pub fn print_output_block(message: &str) {
    let message = if is_plain() {
        message.to_string()
    } else {
        indent_output(message)
    };
    println!("\n{message}\n");
}

fn indent_output(message: &str) -> String {
    message
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{OUTPUT_INDENT}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_command(cmd: &str, use_color: bool) -> String {
    let name = command_name();
    let full = if cmd.is_empty() {
        name.to_string()
    } else {
        format!("{name} {cmd}")
    };
    format_cmd(&full, use_color)
}

fn format_save_hint(paths: &Paths, use_color: bool, save_only: &str, with_login: &str) -> String {
    format_hint(
        &format_save_hint_message(paths, use_color, save_only, with_login),
        use_color,
    )
}

fn format_save_hint_message(
    paths: &Paths,
    use_color: bool,
    save_only: &str,
    with_login: &str,
) -> String {
    let save = format_command("save", use_color);
    if has_auth(&paths.auth) {
        save_only.replace("{save}", &save)
    } else {
        let login = format_cmd("codex login", use_color);
        with_login
            .replace("{login}", &login)
            .replace("{save}", &save)
    }
}

/// Strip ANSI escape sequences from a string. Handles all `ESC [` sequences
/// (not just SGR `m`-terminated ones) so cursor movement codes are also removed.
pub(crate) fn strip_ansi(input: &str) -> String {
    if !input.contains('\u{1b}') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    if (0x40..=0x7e).contains(&b) {
                        break;
                    }
                }
            }
            continue;
        }
        let ch = input[i..].chars().next().expect("valid utf-8 char");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{ENV_MUTEX, make_paths, set_env_guard, set_plain_guard, spawn_server};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    #[test]
    fn plain_toggle_affects_output() {
        {
            let _plain = set_plain_guard(true);
            assert!(is_plain());
            let warning = format_warning("oops", false);
            assert!(warning.contains("Warning"));
        }
        assert!(!is_plain());
    }

    #[test]
    fn format_warning_multiline_aligns_continuation() {
        let message = format!(
            "{}\n{}",
            crate::AUTH_REFRESH_401_TITLE,
            crate::AUTH_RELOGIN_AND_SAVE
        );
        let warning = format_warning(&message, false);
        let expected = format!(
            "Warning: {}\n         {}",
            crate::AUTH_REFRESH_401_TITLE,
            crate::AUTH_RELOGIN_AND_SAVE
        );
        assert_eq!(warning, expected);
    }

    #[test]
    fn supports_color_respects_no_color() {
        let _env = set_env_guard("NO_COLOR", Some("1"));
        assert!(!use_color_stdout());
        assert!(!use_color_stderr());
    }

    #[test]
    fn format_helpers_basic() {
        let _plain = set_plain_guard(false);
        let cmd = format_cmd("codex login", false);
        assert!(cmd.contains("codex login"));
        let action = format_action("done", false);
        assert!(action.contains("done"));
        let hint = format_hint("hint", false);
        assert!(hint.contains("hint"));
        let cancel = format_cancel(false);
        assert_eq!(cancel, CANCELLED_MESSAGE);
    }

    #[test]
    fn format_no_profiles_and_save_before_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        let msg = format_no_profiles(&paths, false);
        assert!(msg.contains("No saved profiles"));
        let msg = format_save_before_load_or_force(&paths, false);
        assert!(msg.contains("save"));
        assert!(msg.contains("--force"));
    }

    #[test]
    fn format_unsaved_warning_plain() {
        let lines = format_unsaved_warning(false);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("Warning"));
    }

    #[test]
    fn normalize_error_variants() {
        assert_eq!(
            normalize_error("Error: Codex auth file not found. Run `codex login` first."),
            "Not logged in. Run `codex login`."
        );
        assert_eq!(
            normalize_error(
                "Error: invalid JSON in auth.json: oops. Run `codex login` to regenerate it."
            ),
            "Auth file is invalid. Run `codex login`."
        );
        assert_eq!(
            normalize_error(
                "Error: auth.json is missing tokens.account_id. Run `codex login` to reauthenticate."
            ),
            "Auth is incomplete. Run `codex login`."
        );
        assert_eq!(normalize_error("other"), "other");
    }

    #[test]
    fn format_error_plain() {
        let _env = set_env_guard("NO_COLOR", Some("1"));
        let err = format_error("oops");
        assert!(err.contains("Error:"));
    }

    #[test]
    fn format_error_multiline_aligns_continuation() {
        let _env = set_env_guard("NO_COLOR", Some("1"));
        let message = crate::msg2(
            crate::UI_ERROR_TWO_LINE,
            crate::AUTH_REFRESH_401_TITLE,
            crate::AUTH_RELOGIN_AND_SAVE,
        );
        let err = format_error(&message);
        assert_eq!(
            err,
            "Error: Token refresh unauthorized (401)\n       Authenticate again with `codex login`, then save this profile"
        );
    }

    #[test]
    fn format_profile_display_variants() {
        let key = format_profile_display(
            Some("Key".to_string()),
            Some("Key".to_string()),
            Some("label".to_string()),
            false,
            false,
        );
        assert!(key.to_lowercase().contains("key"));
        let display = format_profile_display(
            Some("me@example.com".to_string()),
            Some("Free".to_string()),
            None,
            true,
            false,
        );
        assert!(display.contains("me@example.com"));
        let unknown = format_profile_display(None, None, None, false, false);
        assert!(unknown.contains("Unknown"));
    }

    #[test]
    fn format_profile_display_sanitizes_terminal_control_sequences() {
        let display = format_profile_display(
            Some("me\u{1b}[31m@example.com\u{1b}[0m".to_string()),
            Some("pro\u{7}".to_string()),
            Some("la\u{1b}]8;;https://evil\u{7}bel\u{1b}]8;;\u{7}".to_string()),
            false,
            false,
        );
        assert!(!display.contains('\u{1b}'));
        assert!(!display.contains('\u{7}'));
        assert!(display.contains("me@example.com"));
        assert!(display.contains("PRO"));
        assert!(display.contains("label"));
    }

    #[test]
    fn format_entry_header_sanitizes_terminal_control_sequences() {
        let header = format_entry_header("foo\u{1b}[31mbar\u{1b}[0m", false);
        assert_eq!(header, "foobar");
    }

    #[test]
    fn format_entry_header_preserves_existing_ansi_sequences_when_colored() {
        colored::control::set_override(true);
        let header = format_entry_header("\u{1b}[32mfoo\u{1b}[0m", true);
        colored::control::unset_override();
        assert!(header.contains("\u{1b}[32m"));
        assert_eq!(strip_ansi(&header), "foo");
    }

    #[test]
    fn format_entry_header_and_separator() {
        let header = format_entry_header("Display", false);
        assert!(header.contains("Display"));
        let indented = super::indent_output("line\n\nline2");
        assert!(indented.contains("line2"));
    }

    #[test]
    fn render_config_and_cancel() {
        let _env = set_env_guard("NO_COLOR", Some("1"));
        let config = inquire_select_render_config();
        assert_eq!(config.prompt_prefix.content, "");
        let err = inquire::error::InquireError::OperationCanceled;
        assert!(is_inquire_cancel(&err));
    }

    #[test]
    fn print_output_blocks() {
        let _plain = set_plain_guard(true);
        print_output_block("hi");
    }

    #[test]
    fn format_command_uses_name() {
        let cmd = super::format_command("list", false);
        assert!(cmd.contains("list"));
    }

    #[test]
    fn format_save_hint_with_auth() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::write(&paths.auth, "{}").expect("write auth");
        let hint = super::format_save_hint(&paths, false, "Run {save}", "Run {login} {save}");
        assert!(hint.contains("save"));
    }

    #[test]
    fn colored_and_edge_rendering_branches() {
        let _env_lock = ENV_MUTEX.lock().unwrap();
        let _force_color = set_env_guard("FORCE_COLOR", Some("1"));
        let _no_color = set_env_guard("NO_COLOR", None);
        let _plain = set_plain_guard(false);

        assert!(use_color_stdout());
        assert!(use_color_stderr());
        assert!(style_text("text", true, |text| text.bold()).contains("text"));
        assert!(format_action("done", true).contains("done"));
        assert!(format_cancel(true).contains(CANCELLED_MESSAGE));
        assert!(format_warning("first\nsecond", true).contains("second"));
        assert!(format_hint("hint", true).contains("hint"));
        assert_eq!(format_unsaved_warning(true).len(), 2);
        assert!(format_error("oops\nmore").contains("Error:"));
        assert!(
            format_profile_display(
                Some("me@example.com".to_string()),
                Some("Pro".to_string()),
                Some("work".to_string()),
                true,
                true,
            )
            .contains("me@example.com")
        );
        assert!(
            format_profile_display(Some("me@example.com".to_string()), None, None, false, true,)
                .contains("me@example.com")
        );
        let dir = tempfile::tempdir().unwrap();
        let paths = make_paths(dir.path());
        assert!(format_no_profiles(&paths, true).contains("codex login"));
        let _ = inquire_select_render_config();
        assert!(is_inquire_cancel(
            &inquire::error::InquireError::OperationInterrupted
        ));
        assert!(format_command("", false).contains(command_name()));
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\u{1b}X"), "X");
    }

    #[test]
    fn nested_plain_guard_and_spawn_server_write_response() {
        {
            let _outer = set_plain_guard(false);
            let _inner = set_plain_guard(true);
            assert!(is_plain());
        }
        assert!(!is_plain());

        let endpoint = spawn_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_string());
        let authority = endpoint.strip_prefix("http://").unwrap();
        let mut stream = TcpStream::connect(authority).unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("200 OK"));
        assert!(response.ends_with("ok"));
    }
}
