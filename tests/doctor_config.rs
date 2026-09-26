use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

static COMMAND_MUTEX: Mutex<()> = Mutex::new(());

struct TestEnv {
    home: tempfile::TempDir,
    binary: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::Builder::new()
            .prefix("codex-profiles-doctor-config-")
            .tempdir()
            .expect("create temp home");
        fs::create_dir_all(home.path().join(".codex")).expect("create codex home");

        let binary = env::var_os("CARGO_BIN_EXE_codex-profiles")
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .unwrap_or_else(|| {
                let current = env::current_exe().expect("current test executable");
                current
                    .parent()
                    .and_then(Path::parent)
                    .expect("target directory")
                    .join(if cfg!(windows) {
                        "codex-profiles.exe"
                    } else {
                        "codex-profiles"
                    })
            });

        Self { home, binary }
    }

    fn codex(&self) -> PathBuf {
        self.home.path().join(".codex")
    }

    fn write_config(&self, contents: &str) -> PathBuf {
        let path = self.codex().join("config.toml");
        fs::write(&path, contents).expect("write config");
        path
    }

    fn write_api_key_auth(&self, api_key: &str) {
        let value = serde_json::json!({ "OPENAI_API_KEY": api_key });
        fs::write(
            self.codex().join("auth.json"),
            serde_json::to_vec(&value).expect("serialize api key auth"),
        )
        .expect("write api key auth");
    }

    fn run(&self, args: &[&str]) -> Output {
        let _guard = COMMAND_MUTEX.lock().expect("command mutex");
        Command::new(&self.binary)
            .args(args)
            .env_remove("CODEX_HOME")
            .env("HOME", self.home.path())
            .env("CODEX_PROFILES_HOME", self.home.path())
            .env("CODEX_PROFILES_COMMAND", "codex-profiles")
            .env("CODEX_PROFILES_SKIP_UPDATE", "1")
            .env("NO_COLOR", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .expect("run doctor")
    }

    fn stdout(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "doctor failed: {:?}\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("doctor output is UTF-8")
    }
}

#[test]
fn codex_config_reports_safe_categories_and_legacy_profiles() {
    let env = TestEnv::new();
    let output = env.stdout(&["doctor"]);
    assert!(output.contains("[ok] codex_config: missing"), "{output}");
    assert!(output.contains("auth storage=file"), "{output}");
    assert!(
        output.contains("ChatGPT endpoint=official default"),
        "{output}"
    );
    assert!(output.contains(
        "user config only; managed requirements and CLI/environment overrides are not evaluated"
    ));

    env.write_config(
        r#"cli_auth_credentials_store = "file"
chatgpt_base_url = "https://chatgpt.com/backend-api/"
profile = "legacy"
[profiles.work]
model = "private-model-name"
"#,
    );
    let output = env.stdout(&["doctor"]);
    assert!(output.contains("[warn] codex_config: valid"), "{output}");
    assert!(
        output.contains("auth storage=file (compatible)"),
        "{output}"
    );
    assert!(
        output.contains("ChatGPT endpoint=official host"),
        "{output}"
    );
    assert!(
        output.contains("legacy native profile settings detected"),
        "{output}"
    );
    assert!(!output.contains("private-model-name"), "{output}");
}

#[test]
fn codex_config_redacts_invalid_toml_and_custom_values() {
    let env = TestEnv::new();
    env.write_config(
        "chatgpt_base_url = \"https://secret.example/backend-api/?token=private-value\"\ninvalid = [\n",
    );
    let output = env.stdout(&["doctor"]);
    assert!(
        output.contains("[error] codex_config: invalid TOML"),
        "{output}"
    );
    assert!(!output.contains("secret.example"), "{output}");
    assert!(!output.contains("private-value"), "{output}");

    env.write_config(
        "chatgpt_base_url = \"https://secret.example/backend-api/?token=private-value\"\n",
    );
    let output = env.stdout(&["doctor"]);
    assert!(
        output.contains("ChatGPT endpoint=unrecognized host"),
        "{output}"
    );
    assert!(!output.contains("secret.example"), "{output}");
    assert!(!output.contains("private-value"), "{output}");

    env.write_config("chatgpt_base_url = \"http://[\"\n");
    let output = env.stdout(&["doctor"]);
    assert!(
        output.contains("ChatGPT endpoint=invalid or unsupported"),
        "{output}"
    );

    env.write_config("chatgpt_base_url = \"http://127.0.0.2:43123/backend-api\"\n");
    let output = env.stdout(&["doctor", "--json"]);
    assert!(
        output.contains("ChatGPT endpoint=loopback host"),
        "{output}"
    );
}

#[test]
fn doctor_does_not_echo_an_unsupported_store_value_from_other_checks() {
    let env = TestEnv::new();
    env.write_config("cli_auth_credentials_store = \"private-store-token\"\n");
    fs::write(env.codex().join("auth.json"), "{}\n").expect("write auth fixture");

    let output = env.stdout(&["doctor", "--json"]);
    assert!(
        output.contains("\"name\": \"codex_config\"")
            && output.contains("\"level\": \"error\"")
            && output.contains("valid; auth storage setting is invalid"),
        "{output}"
    );
    assert!(
        output.contains("auth storage setting is invalid"),
        "{output}"
    );
    assert!(
        output.contains("unsupported credential-store mode"),
        "{output}"
    );
    assert!(!output.contains("private-store-token"), "{output}");
}

#[test]
fn codex_config_reports_unsupported_storage_without_fallback() {
    let env = TestEnv::new();
    env.write_config("cli_auth_credentials_store = \"auto\"\n");

    let output = env.stdout(&["doctor", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse doctor JSON");
    let check = json["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|check| check["name"] == "codex_config")
        .expect("codex_config check");
    let detail = check["detail"].as_str().expect("check detail");
    assert_eq!(check["level"], "warn");
    assert!(
        detail.contains("auth storage=auto (unsupported"),
        "{detail}"
    );
    assert!(
        detail.contains("auth.json is not authoritative"),
        "{detail}"
    );
}

#[test]
fn doctor_fix_never_rewrites_config_toml() {
    let env = TestEnv::new();
    let config = env.write_config(
        "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"http://127.0.0.1:43123/backend-api\"\n",
    );
    let before = fs::read(&config).expect("read config before doctor --fix");
    let output = env.stdout(&["doctor", "--fix"]);
    let after = fs::read(&config).expect("read config after doctor --fix");
    assert_eq!(before, after, "doctor --fix changed config.toml: {output}");
    assert!(output.contains("codex_config"), "{output}");
}

#[test]
fn doctor_inspects_existing_profile_storage_and_active_profile() {
    let env = TestEnv::new();
    env.write_config(
        "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"https://chatgpt.com/backend-api\"\n",
    );
    env.write_api_key_auth("sk-doctor-coverage-test");
    env.stdout(&["save", "--label", "coverage"]);

    let output = env.stdout(&["doctor", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&output).expect("parse doctor JSON");
    let checks = json["checks"].as_array().expect("checks array");
    let detail = |name: &str| {
        checks
            .iter()
            .find(|check| check["name"] == name)
            .and_then(|check| check["detail"].as_str())
            .unwrap_or_else(|| panic!("missing doctor check {name}: {output}"))
    };

    assert!(detail("codex_config").contains("auth storage=file (compatible)"));
    assert!(detail("auth file").starts_with("valid"));
    assert_eq!(
        detail("profiles directory"),
        env.codex().join("profiles").display().to_string()
    );
    assert!(detail("profiles index").contains("1 entries"));
    assert_eq!(detail("profiles lock"), "acquired");
    assert_eq!(detail("saved profiles"), "1 valid, 0 invalid");
    assert_eq!(detail("active profile"), "saved");
    assert!(!output.contains("sk-doctor-coverage-test"), "{output}");

    let config = env.codex().join("config.toml");
    let before = fs::read(&config).expect("read config before fix");
    let fixed = env.stdout(&["doctor", "--fix", "--json"]);
    let after = fs::read(&config).expect("read config after fix");
    assert_eq!(before, after, "doctor --fix changed config.toml: {fixed}");
}
