use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use directories::BaseDirs;
use serde_json::Value;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[cfg(test)]
use std::cell::Cell;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use crate::COMMON_ERR_SET_PERMISSIONS;
use crate::{
    COMMON_ERR_CREATE_DIR, COMMON_ERR_CREATE_PROFILES_DIR, COMMON_ERR_CREATE_TEMP,
    COMMON_ERR_EXISTS_NOT_DIR, COMMON_ERR_EXISTS_NOT_FILE, COMMON_ERR_GET_TIME,
    COMMON_ERR_INVALID_FILE_NAME, COMMON_ERR_READ_FILE, COMMON_ERR_READ_METADATA,
    COMMON_ERR_REPLACE_FILE, COMMON_ERR_RESOLVE_HOME, COMMON_ERR_RESOLVE_PARENT,
    COMMON_ERR_SET_TEMP_PERMISSIONS, COMMON_ERR_WRITE_LOCK_FILE, COMMON_ERR_WRITE_TEMP,
};

const UNEXPECTED_HTTP_BODY_MAX_BYTES: usize = 1000;

pub struct Paths {
    pub codex: PathBuf,
    pub auth: PathBuf,
    pub profiles: PathBuf,
    pub profiles_index: PathBuf,
    pub update_cache: PathBuf,
    pub profiles_lock: PathBuf,
}

pub fn command_name() -> &'static str {
    static COMMAND_NAME: OnceLock<String> = OnceLock::new();
    COMMAND_NAME
        .get_or_init(|| {
            let env_value = env::var("CODEX_PROFILES_COMMAND").ok();
            compute_command_name_from(env_value, env::args_os())
        })
        .as_str()
}

fn compute_command_name_from<I>(env_value: Option<String>, mut args: I) -> String
where
    I: Iterator<Item = std::ffi::OsString>,
{
    if let Some(value) = env_value {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    args.next()
        .and_then(|arg| {
            Path::new(&arg)
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_string())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "codex-profiles".to_string())
}

pub fn package_command_name() -> &'static str {
    "codex-profiles"
}

#[cfg(unix)]
pub(crate) const FAIL_SET_PERMISSIONS: usize = 1;
pub(crate) const FAIL_WRITE_OPEN: usize = 2;
pub(crate) const FAIL_WRITE_WRITE: usize = 3;
pub(crate) const FAIL_WRITE_PERMS: usize = 4;
pub(crate) const FAIL_WRITE_SYNC: usize = 5;
pub(crate) const FAIL_WRITE_RENAME: usize = 6;
pub(crate) const FAIL_FILE_IDENTITY: usize = 7;

#[cfg(test)]
type AtomicCommitHook = fn(&Path, &Path);

#[cfg(test)]
thread_local! {
    static FAILPOINT: Cell<usize> = const { Cell::new(0) };
    static FAILPOINT_REMAINING: Cell<usize> = const { Cell::new(0) };
    static BEFORE_ATOMIC_COMMIT: Cell<Option<AtomicCommitHook>> = const { Cell::new(None) };
    static BEFORE_ATOMIC_COMMIT_REMAINING: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) struct AtomicCommitHookGuard {
    previous: Option<AtomicCommitHook>,
    previous_remaining: usize,
}

#[cfg(test)]
impl AtomicCommitHookGuard {
    pub(crate) fn new(hook: AtomicCommitHook) -> Self {
        Self::on_nth_commit(hook, 1)
    }

    pub(crate) fn on_nth_commit(hook: AtomicCommitHook, hit: usize) -> Self {
        Self {
            previous: BEFORE_ATOMIC_COMMIT.with(|slot| slot.replace(Some(hook))),
            previous_remaining: BEFORE_ATOMIC_COMMIT_REMAINING
                .with(|remaining| remaining.replace(hit.saturating_sub(1))),
        }
    }
}

#[cfg(test)]
impl Drop for AtomicCommitHookGuard {
    fn drop(&mut self) {
        BEFORE_ATOMIC_COMMIT.with(|slot| slot.set(self.previous));
        BEFORE_ATOMIC_COMMIT_REMAINING.with(|remaining| remaining.set(self.previous_remaining));
    }
}

#[cfg(test)]
fn maybe_fail(step: usize) -> std::io::Result<()> {
    if FAILPOINT.with(|failpoint| failpoint.get()) == step {
        let remaining = FAILPOINT_REMAINING.with(|remaining| remaining.get());
        if remaining > 0 {
            FAILPOINT_REMAINING.with(|remaining| remaining.set(remaining.get() - 1));
            return Ok(());
        }
        return Err(std::io::Error::other("failpoint"));
    }
    Ok(())
}

#[cfg(not(test))]
fn maybe_fail(_step: usize) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
pub(crate) struct FailpointGuard {
    previous_step: usize,
    previous_remaining: usize,
}

#[cfg(test)]
impl FailpointGuard {
    pub(crate) fn new(step: usize, hit: usize) -> Self {
        let previous_step = FAILPOINT.with(|failpoint| {
            let previous = failpoint.get();
            failpoint.set(step);
            previous
        });
        let previous_remaining = FAILPOINT_REMAINING.with(|remaining| {
            let previous = remaining.get();
            remaining.set(hit.saturating_sub(1));
            previous
        });
        Self {
            previous_step,
            previous_remaining,
        }
    }
}

#[cfg(test)]
impl Drop for FailpointGuard {
    fn drop(&mut self) {
        FAILPOINT.with(|failpoint| failpoint.set(self.previous_step));
        FAILPOINT_REMAINING.with(|remaining| remaining.set(self.previous_remaining));
    }
}

pub fn resolve_paths() -> Result<Paths, String> {
    let codex_home = env::var_os("CODEX_HOME").map(PathBuf::from);
    let home = resolve_home_dir();
    let codex_dir = resolve_codex_dir_with(codex_home, home)?;
    let auth = codex_dir.join("auth.json");
    let profiles = codex_dir.join("profiles");
    let profiles_index = profiles.join("profiles.json");
    let update_cache = profiles.join("update.json");
    let profiles_lock = profiles.join("profiles.lock");
    Ok(Paths {
        codex: codex_dir,
        auth,
        profiles,
        profiles_index,
        update_cache,
        profiles_lock,
    })
}

fn resolve_codex_dir_with(
    codex_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Some(path) = non_empty_path(codex_home) {
        let metadata = fs::metadata(&path)
            .map_err(|err| format!("Cannot read CODEX_HOME {}: {err}", path.display()))?;
        if !metadata.is_dir() {
            return Err(format!("CODEX_HOME {} is not a directory", path.display()));
        }
        return canonicalize_codex_dir(&path);
    }
    home.map(|path| path.join(".codex"))
        .ok_or_else(|| COMMON_ERR_RESOLVE_HOME.to_string())
}

fn canonicalize_codex_dir(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|err| format!("Cannot canonicalize CODEX_HOME {}: {err}", path.display()))
}

/// Read only a root-level string setting. Never include configuration contents
/// in errors: provider configuration may contain credentials.
pub(crate) fn read_config_string(path: &Path, keys: &[&str]) -> Result<Option<String>, String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(format!(
                "Cannot read configuration {}: {err}",
                path.display()
            ));
        }
    };
    let config: toml::Table = contents
        .parse()
        .map_err(|_| format!("Invalid TOML configuration in {}", path.display()))?;
    for key in keys {
        if let Some(value) = config.get(*key) {
            return value
                .as_str()
                .map(|value| Some(value.to_string()))
                .ok_or_else(|| {
                    format!(
                        "Configuration setting {key} must be a string in {}",
                        path.display()
                    )
                });
        }
    }
    Ok(None)
}

fn resolve_home_dir() -> Option<PathBuf> {
    let codex_home = env::var_os("CODEX_PROFILES_HOME").map(PathBuf::from);
    let base_home = BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    let home = env::var_os("HOME").map(PathBuf::from);
    let userprofile = env::var_os("USERPROFILE").map(PathBuf::from);
    let homedrive = env::var_os("HOMEDRIVE").map(PathBuf::from);
    let homepath = env::var_os("HOMEPATH").map(PathBuf::from);
    resolve_home_dir_with(
        codex_home,
        base_home,
        home,
        userprofile,
        homedrive,
        homepath,
    )
}

fn resolve_home_dir_with(
    codex_home: Option<PathBuf>,
    base_home: Option<PathBuf>,
    home: Option<PathBuf>,
    userprofile: Option<PathBuf>,
    homedrive: Option<PathBuf>,
    homepath: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(path) = non_empty_path(codex_home) {
        return Some(path);
    }
    if let Some(path) = base_home {
        return Some(path);
    }
    if let Some(path) = non_empty_path(home) {
        return Some(path);
    }
    if let Some(path) = non_empty_path(userprofile) {
        return Some(path);
    }
    match (homedrive, homepath) {
        (Some(drive), Some(path)) => {
            let mut out = drive;
            out.push(path);
            if out.as_os_str().is_empty() {
                None
            } else {
                Some(out)
            }
        }
        _ => None,
    }
}

fn non_empty_path(path: Option<PathBuf>) -> Option<PathBuf> {
    path.filter(|path| !path.as_os_str().is_empty())
}

pub fn ensure_paths(paths: &Paths) -> Result<(), String> {
    if paths.profiles.exists() && !paths.profiles.is_dir() {
        return Err(crate::msg1(
            COMMON_ERR_EXISTS_NOT_DIR,
            paths.profiles.display(),
        ));
    }

    fs::create_dir_all(&paths.profiles).map_err(|err| {
        crate::msg2(
            COMMON_ERR_CREATE_PROFILES_DIR,
            paths.profiles.display(),
            err,
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o700);
        if let Err(err) = set_profile_permissions(&paths.profiles, perms) {
            return Err(crate::msg2(
                COMMON_ERR_SET_PERMISSIONS,
                paths.profiles.display(),
                err,
            ));
        }
    }

    ensure_file_or_absent(&paths.profiles_index)?;
    ensure_file_or_absent(&paths.update_cache)?;
    ensure_file_or_absent(&paths.profiles_lock)?;

    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&paths.profiles_lock).map_err(|err| {
        crate::msg2(
            COMMON_ERR_WRITE_LOCK_FILE,
            paths.profiles_lock.display(),
            err,
        )
    })?;

    Ok(())
}

pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), String> {
    let permissions = fs::metadata(path).ok().map(|meta| meta.permissions());
    write_atomic_with_permissions(path, contents, permissions)
}

#[cfg(unix)]
pub fn write_atomic_with_mode(path: &Path, contents: &[u8], mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = fs::Permissions::from_mode(mode);
    write_atomic_with_permissions(path, contents, Some(permissions))
}

pub fn write_atomic_private(path: &Path, contents: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        write_atomic_with_mode(path, contents, 0o600)
    }
    #[cfg(not(unix))]
    {
        write_atomic(path, contents)
    }
}

#[derive(Debug)]
pub(crate) enum AtomicCreateError {
    AlreadyExists,
    Write(String),
}

pub(crate) struct CreatedFile {
    // Keep the original file open so its identity cannot be recycled after a
    // concurrent replacement. This receipt never deletes anything on drop.
    _file: fs::File,
    identity: FileIdentity,
}

#[derive(PartialEq, Eq)]
struct FileIdentity {
    volume: u64,
    file: u128,
}

fn file_identity(file: &fs::File) -> std::io::Result<FileIdentity> {
    maybe_fail(FAIL_FILE_IDENTITY)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(FileIdentity {
            volume: metadata.dev(),
            file: metadata.ino().into(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ID_128, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
        };
        let mut info = FILE_ID_INFO {
            VolumeSerialNumber: 0,
            FileId: FILE_ID_128 {
                Identifier: [0; 16],
            },
        };
        // SAFETY: the borrowed File keeps the handle live throughout this call.
        // FileIdInfo writes exactly FILE_ID_INFO into the correctly aligned,
        // initialized buffer, whose full size is supplied. No pointer escapes.
        let success = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&mut info as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if success == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(FileIdentity {
            volume: info.VolumeSerialNumber,
            file: u128::from_le_bytes(info.FileId.Identifier),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "file identity is unavailable on this platform",
        ))
    }
}

impl CreatedFile {
    /// Rollback under the caller's store lock, preserving detected replacements
    /// and edits. The final identity check and unlink are separate operations;
    /// a noncooperating writer can still race them. See docs/compatibility.md.
    pub(crate) fn remove_if_unchanged(
        &self,
        path: &Path,
        expected: &[u8],
    ) -> std::io::Result<bool> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(err) => return Err(err),
        };
        if !metadata.is_file() || metadata.len() != expected.len() as u64 {
            return Ok(false);
        }
        let current = fs::File::open(path)?;
        if file_identity(&current)? != self.identity {
            return Ok(false);
        }
        let mut contents = Vec::new();
        (&current)
            .take(expected.len().saturating_add(1) as u64)
            .read_to_end(&mut contents)?;
        if contents != expected
            || !fs::symlink_metadata(path)?.is_file()
            || file_identity(&fs::File::open(path)?)? != self.identity
        {
            return Ok(false);
        }
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(err) => Err(err),
        }
    }
}

/// Publish a complete private file without replacing any existing directory entry.
/// The destination filesystem must support hard links; failures never fall back
/// to a replacing rename or a partially written destination.
pub(crate) fn create_atomic_private(path: &Path, contents: &[u8]) -> Result<(), AtomicCreateError> {
    create_atomic_private_with(path, contents, |file| {
        drop(file);
        Ok(())
    })
}

pub(crate) fn create_atomic_private_tracked(
    path: &Path,
    contents: &[u8],
) -> Result<CreatedFile, AtomicCreateError> {
    create_atomic_private_with(path, contents, |file| {
        let identity = file_identity(&file)?;
        Ok(CreatedFile {
            _file: file,
            identity,
        })
    })
}

fn create_atomic_private_with<T>(
    path: &Path,
    contents: &[u8],
    capture: impl FnOnce(fs::File) -> std::io::Result<T>,
) -> Result<T, AtomicCreateError> {
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions = None;

    let (pending, file) =
        prepare_atomic_write(path, contents, permissions).map_err(AtomicCreateError::Write)?;
    let receipt = capture(file).map_err(|err| {
        AtomicCreateError::Write(crate::msg2(COMMON_ERR_READ_METADATA, path.display(), err))
    })?;
    // Linking in the same directory makes the destination appear only when the
    // complete file is ready, and atomically refuses an occupied name.
    match maybe_fail(FAIL_WRITE_RENAME).and_then(|_| fs::hard_link(&pending.0, path)) {
        Ok(()) => Ok(receipt),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(AtomicCreateError::AlreadyExists)
        }
        Err(err) => Err(AtomicCreateError::Write(crate::msg2(
            COMMON_ERR_REPLACE_FILE,
            path.display(),
            err,
        ))),
    }
}

struct PendingAtomicWrite(PathBuf);

impl Drop for PendingAtomicWrite {
    fn drop(&mut self) {
        // A rename consumes this path; a hard link leaves it for cleanup.
        // On any earlier failure, remove the partial credential file.
        let _ = fs::remove_file(&self.0);
    }
}

fn write_atomic_with_permissions(
    path: &Path,
    contents: &[u8],
    permissions: Option<fs::Permissions>,
) -> Result<(), String> {
    let (pending, file) = prepare_atomic_write(path, contents, permissions)?;
    drop(file);
    // Never delete the destination after a failed replacement: it may be the
    // only valid auth cache.
    maybe_fail(FAIL_WRITE_RENAME)
        .and_then(|_| fs::rename(&pending.0, path))
        .map_err(|err| crate::msg2(COMMON_ERR_REPLACE_FILE, path.display(), err))
}

fn prepare_atomic_write(
    path: &Path,
    contents: &[u8],
    permissions: Option<fs::Permissions>,
) -> Result<(PendingAtomicWrite, fs::File), String> {
    let parent = path
        .parent()
        .ok_or_else(|| crate::msg1(COMMON_ERR_RESOLVE_PARENT, path.display()))?;
    let _ = (!parent.as_os_str().is_empty())
        .then(|| fs::create_dir_all(parent))
        .transpose()
        .map_err(|err| crate::msg2(COMMON_ERR_CREATE_DIR, parent.display(), err))?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| crate::msg1(COMMON_ERR_INVALID_FILE_NAME, path.display()))?;
    let pid = std::process::id();
    let mut attempt = 0u32;
    loop {
        let nanos = temporary_file_timestamp(SystemTime::now())?;
        let tmp_name = format!(".{file_name}.tmp-{pid}-{nanos}-{attempt}");
        let tmp_path = parent.join(tmp_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(permissions) = permissions.as_ref() {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(permissions.mode());
        }
        let file = match options.open(&tmp_path) {
            Ok(file) => file,
            Err(err) => {
                attempt += 1;
                if attempt < 5 {
                    continue;
                }
                return Err(crate::msg2(COMMON_ERR_CREATE_TEMP, path.display(), err));
            }
        };
        // This declaration order closes the handle before cleanup on errors.
        // Replacement closes it before renaming; creation retains an ownership
        // receipt through publication and any subsequent rollback.
        let pending = PendingAtomicWrite(tmp_path.clone());
        let mut tmp_file = file;
        maybe_fail(FAIL_WRITE_OPEN)
            .map_err(|err| crate::msg2(COMMON_ERR_CREATE_TEMP, path.display(), err))?;

        maybe_fail(FAIL_WRITE_WRITE)
            .and_then(|_| tmp_file.write_all(contents))
            .map_err(|err| crate::msg2(COMMON_ERR_WRITE_TEMP, path.display(), err))?;

        if let Some(permissions) = permissions {
            maybe_fail(FAIL_WRITE_PERMS)
                .and_then(|_| fs::set_permissions(&tmp_path, permissions))
                .map_err(|err| crate::msg2(COMMON_ERR_SET_TEMP_PERMISSIONS, path.display(), err))?;
        }

        maybe_fail(FAIL_WRITE_SYNC)
            .and_then(|_| tmp_file.sync_all())
            .map_err(|err| crate::msg2(COMMON_ERR_WRITE_TEMP, path.display(), err))?;

        #[cfg(test)]
        {
            let remaining = BEFORE_ATOMIC_COMMIT_REMAINING.with(Cell::get);
            if remaining > 0 {
                BEFORE_ATOMIC_COMMIT_REMAINING.with(|slot| slot.set(remaining - 1));
            } else if let Some(hook) = BEFORE_ATOMIC_COMMIT.with(Cell::take) {
                hook(&tmp_path, path);
            }
        }

        return Ok((pending, tmp_file));
    }
}

fn temporary_file_timestamp(now: SystemTime) -> Result<u128, String> {
    now.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|err| crate::msg1(COMMON_ERR_GET_TIME, err))
}

pub fn copy_atomic(source: &Path, dest: &Path) -> Result<(), String> {
    let permissions = fs::metadata(source)
        .map_err(|err| crate::msg2(COMMON_ERR_READ_METADATA, source.display(), err))?
        .permissions();
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = permissions;
        permissions.set_mode(0o600);
        permissions
    };
    let contents =
        fs::read(source).map_err(|err| crate::msg2(COMMON_ERR_READ_FILE, source.display(), err))?;
    write_atomic_with_permissions(dest, &contents, Some(permissions))
}

fn ensure_file_or_absent(path: &Path) -> Result<(), String> {
    if path.exists() && !path.is_file() {
        return Err(crate::msg1(COMMON_ERR_EXISTS_NOT_FILE, path.display()));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct UnexpectedHttpError {
    status: u16,
    status_text: Option<String>,
    body: String,
    url: Option<String>,
    cf_ray: Option<String>,
    request_id: Option<String>,
    identity_authorization_error: Option<String>,
    identity_error_code: Option<String>,
}

impl UnexpectedHttpError {
    pub fn from_ureq_response(
        response: ureq::http::Response<ureq::Body>,
        url: Option<&str>,
    ) -> Self {
        let status = response.status();
        let request_id = header_value(&response, "x-request-id")
            .or_else(|| header_value(&response, "x-oai-request-id"));
        let cf_ray = header_value(&response, "cf-ray");
        let identity_authorization_error = header_value(&response, "x-openai-authorization-error");
        let identity_error_code = header_value(&response, "x-error-json")
            .and_then(|value| decode_identity_error_code(&value));
        let body = response.into_body().read_to_string().unwrap_or_default();
        Self {
            status: status.as_u16(),
            status_text: status.canonical_reason().map(str::to_string),
            body,
            url: url.map(str::to_string),
            cf_ray,
            request_id,
            identity_authorization_error,
            identity_error_code,
        }
    }

    pub fn status_code(&self) -> u16 {
        self.status
    }

    fn status_label(&self) -> String {
        match self.status_text.as_deref() {
            Some(text) if !text.is_empty() => format!("{} {}", self.status, text),
            _ => self.status.to_string(),
        }
    }

    fn extract_error_message(&self) -> Option<String> {
        let json = self.parsed_body_json()?;
        Self::extract_error_message_from_json(&json)
    }

    fn parsed_body_json(&self) -> Option<Value> {
        serde_json::from_str::<Value>(&self.body).ok()
    }

    fn extract_error_message_from_json(json: &Value) -> Option<String> {
        let message = json
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)?
            .trim();
        if message.is_empty() {
            None
        } else {
            Some(message.to_string())
        }
    }

    fn display_body(&self) -> String {
        if let Some(message) = self.extract_error_message() {
            return message;
        }
        let trimmed = self.body.trim();
        if trimmed.is_empty() {
            return "Unknown error".to_string();
        }
        truncate_with_ellipsis(trimmed, UNEXPECTED_HTTP_BODY_MAX_BYTES)
    }

    fn plain_body(&self) -> String {
        let parsed = self.parsed_body_json();
        if let Some(message) = parsed
            .as_ref()
            .and_then(Self::extract_error_message_from_json)
            .or_else(|| parsed.as_ref().and_then(Self::extract_detail_message))
        {
            return sanitize_for_terminal(&message).trim().to_string();
        }
        sanitize_for_terminal(&self.display_body())
            .trim()
            .to_string()
    }

    fn extract_detail_message(json: &Value) -> Option<String> {
        json.get("detail")
            .and_then(|detail| detail.get("message"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty())
            .map(str::to_string)
    }

    fn extract_detail_code(&self) -> Option<String> {
        let json = self.parsed_body_json()?;

        json.get("detail")
            .and_then(|detail| detail.get("code"))
            .and_then(Value::as_str)
            .or_else(|| {
                json.get("error")
                    .and_then(|error| error.get("code"))
                    .and_then(Value::as_str)
            })
            .map(str::to_string)
    }

    fn plain_summary(&self) -> String {
        sanitize_for_terminal(
            &self
                .extract_detail_code()
                .unwrap_or_else(|| self.plain_body()),
        )
        .trim()
        .to_string()
    }

    fn append_debug_context(&self, message: &mut String) {
        if let Some(url) = &self.url {
            message.push_str(&format!(", url: {url}"));
        }
        if let Some(cf_ray) = &self.cf_ray {
            message.push_str(&format!(", cf-ray: {cf_ray}"));
        }
        if let Some(id) = &self.request_id {
            message.push_str(&format!(", request id: {id}"));
        }
        if let Some(auth_error) = &self.identity_authorization_error {
            message.push_str(&format!(", auth error: {auth_error}"));
        }
        if let Some(error_code) = &self.identity_error_code {
            message.push_str(&format!(", auth error code: {error_code}"));
        }
    }

    fn append_debug_context_lines(&self, lines: &mut Vec<String>) {
        if let Some(url) = &self.url {
            lines.push(format!("URL: {}", sanitize_for_terminal(url).trim()));
        }
        if let Some(cf_ray) = &self.cf_ray {
            lines.push(format!("CF-Ray: {}", sanitize_for_terminal(cf_ray).trim()));
        }
        if let Some(id) = &self.request_id {
            lines.push(format!("Request ID: {}", sanitize_for_terminal(id).trim()));
        }
        if let Some(auth_error) = &self.identity_authorization_error {
            lines.push(format!(
                "Auth Error: {}",
                sanitize_for_terminal(auth_error).trim()
            ));
        }
        if let Some(error_code) = &self.identity_error_code {
            lines.push(format!(
                "Auth Error Code: {}",
                sanitize_for_terminal(error_code).trim()
            ));
        }
    }

    pub fn plain_message(&self) -> String {
        let mut lines = vec![self.plain_summary()];
        lines.push(format!("unexpected status {}", self.status_label()));
        self.append_debug_context_lines(&mut lines);
        lines.join("\n")
    }
}

impl std::fmt::Display for UnexpectedHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut message = format!(
            "unexpected status {}: {}",
            self.status_label(),
            self.display_body()
        );
        self.append_debug_context(&mut message);
        f.write_str(&message)
    }
}

impl std::error::Error for UnexpectedHttpError {}

fn header_value(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn decode_identity_error_code(encoded: &str) -> Option<String> {
    let decoded = STANDARD.decode(encoded.trim()).ok()?;
    let json = serde_json::from_slice::<Value>(&decoded).ok()?;
    json.get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn truncate_with_ellipsis(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut cut = max_bytes;
    while !text.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    let mut truncated = text[..cut].to_string();
    truncated.push_str("...");
    truncated
}

pub(crate) fn sanitize_for_terminal(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i >= bytes.len() {
                break;
            }
            match bytes[i] {
                b'[' => {
                    i += 1;
                    while i < bytes.len() {
                        let b = bytes[i];
                        i += 1;
                        if (0x40..=0x7e).contains(&b) {
                            break;
                        }
                    }
                }
                b']' => {
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                _ => {
                    i += 1;
                }
            }
            continue;
        }

        let ch = input[i..].chars().next().expect("valid utf-8 char");
        if !ch.is_control() || matches!(ch, '\n' | '\t') {
            out.push(ch);
        }
        i += ch.len_utf8();
    }

    out
}

#[cfg(unix)]
fn set_profile_permissions(path: &Path, perms: fs::Permissions) -> std::io::Result<()> {
    maybe_fail(FAIL_SET_PERMISSIONS)?;
    fs::set_permissions(path, perms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{ENV_MUTEX, make_paths, set_env_guard, spawn_server};
    use std::ffi::OsString;
    use std::fs;
    use ureq::Agent;

    fn with_failpoint<F: FnOnce()>(step: usize, f: F) {
        let _guard = FailpointGuard::new(step, 1);
        f();
    }

    fn with_failpoint_disabled<F: FnOnce()>(f: F) {
        let _guard = FailpointGuard::new(0, 1);
        f();
    }

    fn http_response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
        let mut response = format!("HTTP/1.1 {status}\r\n");
        for (name, value) in headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str(&format!("Content-Length: {}\r\n\r\n{}", body.len(), body));
        response
    }

    fn fetch_response(url: &str) -> ureq::http::Response<ureq::Body> {
        let agent: Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        agent.get(url).call().unwrap()
    }

    #[test]
    fn compute_command_name_uses_env() {
        let name = compute_command_name_from(Some("mycmd".to_string()), Vec::new().into_iter());
        assert_eq!(name, "mycmd");
    }

    #[test]
    fn compute_command_name_uses_args() {
        let args = vec![OsString::from("/usr/bin/codex-profiles")];
        let name = compute_command_name_from(None, args.into_iter());
        assert_eq!(name, "codex-profiles");
    }

    #[test]
    fn compute_command_name_ignores_blank_env() {
        let args = vec![OsString::from("/usr/local/bin/custom")];
        let name = compute_command_name_from(Some("   ".to_string()), args.into_iter());
        assert_eq!(name, "custom");
    }

    #[test]
    fn compute_command_name_fallback() {
        let name = compute_command_name_from(None, Vec::new().into_iter());
        assert_eq!(name, "codex-profiles");
    }

    #[test]
    fn codex_home_is_used_directly_and_takes_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().to_path_buf();
        let expected = custom.canonicalize().unwrap();
        assert_eq!(
            resolve_codex_dir_with(Some(custom.clone()), Some(PathBuf::from("home"))).unwrap(),
            expected
        );
        assert_eq!(
            resolve_codex_dir_with(Some(custom), None).unwrap(),
            expected
        );
        for value in [None, Some(PathBuf::from(""))] {
            assert_eq!(
                resolve_codex_dir_with(value, Some(PathBuf::from("home"))).unwrap(),
                PathBuf::from("home/.codex")
            );
        }
        assert!(resolve_codex_dir_with(None, None).is_err());
        let missing = dir.path().join("missing");
        assert!(
            resolve_codex_dir_with(Some(missing), None)
                .unwrap_err()
                .contains("CODEX_HOME")
        );
        let file = dir.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(
            resolve_codex_dir_with(Some(file), None)
                .unwrap_err()
                .contains("not a directory")
        );
        assert!(
            canonicalize_codex_dir(&dir.path().join("missing"))
                .unwrap_err()
                .contains("Cannot canonicalize CODEX_HOME")
        );
    }

    #[test]
    fn config_reader_respects_toml_scope_and_redacts_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert_eq!(read_config_string(&path, &["setting"]).unwrap(), None);
        fs::write(
            &path,
            "setting = 'root' # comment\n[nested]\nsetting = 'nested'\n",
        )
        .unwrap();
        assert_eq!(
            read_config_string(&path, &["setting"]).unwrap().as_deref(),
            Some("root")
        );
        fs::write(&path, "[nested]\nsetting = 'nested'\n").unwrap();
        assert_eq!(read_config_string(&path, &["setting"]).unwrap(), None);
        for contents in ["setting = 42", "setting = 'private-secret-value"] {
            fs::write(&path, contents).unwrap();
            let error = read_config_string(&path, &["setting"]).unwrap_err();
            assert!(!error.contains("private-secret-value"));
        }
    }

    #[test]
    fn config_reader_reports_unreadable_configuration_without_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::create_dir(&path).unwrap();

        let error = read_config_string(&path, &["secret"]).unwrap_err();
        assert!(error.contains("Cannot read configuration"));
        assert!(!error.contains("secret"));
    }

    #[test]
    fn resolve_paths_uses_configured_codex_home() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let _env = set_env_guard("CODEX_HOME", Some(dir.path().to_str().unwrap()));

        let paths = resolve_paths().unwrap();
        assert_eq!(paths.codex, dir.path().canonicalize().unwrap());
        assert_eq!(paths.auth, paths.codex.join("auth.json"));
        assert_eq!(paths.profiles, paths.codex.join("profiles"));
    }

    #[test]
    fn resolve_home_dir_prefers_codex_env() {
        let out = resolve_home_dir_with(
            Some(PathBuf::from("/tmp/codex")),
            Some(PathBuf::from("/tmp/base")),
            Some(PathBuf::from("/tmp/home")),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(out, PathBuf::from("/tmp/codex"));
    }

    #[test]
    fn resolve_home_dir_uses_base_dirs() {
        let out = resolve_home_dir_with(
            None,
            Some(PathBuf::from("/tmp/base")),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(out, PathBuf::from("/tmp/base"));
    }

    #[test]
    fn resolve_home_dir_falls_back() {
        let out = resolve_home_dir_with(
            Some(PathBuf::from("")),
            None,
            Some(PathBuf::from("/tmp/home")),
            Some(PathBuf::from("/tmp/user")),
            Some(PathBuf::from("C:")),
            Some(PathBuf::from("/Users")),
        )
        .unwrap();
        assert_eq!(out, PathBuf::from("/tmp/home"));
    }

    #[test]
    fn resolve_home_dir_joins_drive_and_homepath_when_other_sources_missing() {
        let out = resolve_home_dir_with(
            None,
            None,
            None,
            None,
            Some(PathBuf::from("drive")),
            Some(PathBuf::from("home")),
        );
        assert_eq!(out, Some(PathBuf::from("drive/home")));
    }

    #[test]
    fn unexpected_http_error_plain_message_formats_multiline_for_unknown_body() {
        let err = UnexpectedHttpError {
            status: 402,
            status_text: Some("Payment Required".to_string()),
            body: r#"{"detail":{"code":"mystery_error"}}"#.to_string(),
            url: Some("https://example.com/backend-api/wham/usage".to_string()),
            cf_ray: Some("ray-123".to_string()),
            request_id: Some("req-123".to_string()),
            identity_authorization_error: None,
            identity_error_code: None,
        };

        assert_eq!(
            err.plain_message(),
            concat!(
                "mystery_error\n",
                "unexpected status 402 Payment Required\n",
                "URL: https://example.com/backend-api/wham/usage\n",
                "CF-Ray: ray-123\n",
                "Request ID: req-123"
            )
        );
        assert!(err.to_string().contains(", cf-ray: ray-123"));
    }

    #[test]
    fn unexpected_http_error_plain_message_formats_multiline_for_known_code() {
        let err = UnexpectedHttpError {
            status: 402,
            status_text: Some("Payment Required".to_string()),
            body: r#"{"detail":{"code":"deactivated_workspace"}}"#.to_string(),
            url: Some("https://example.com/backend-api/wham/usage".to_string()),
            cf_ray: Some("ray-456".to_string()),
            request_id: Some("req-456".to_string()),
            identity_authorization_error: None,
            identity_error_code: None,
        };

        assert_eq!(
            err.plain_message(),
            concat!(
                "deactivated_workspace\n",
                "unexpected status 402 Payment Required\n",
                "URL: https://example.com/backend-api/wham/usage\n",
                "CF-Ray: ray-456\n",
                "Request ID: req-456"
            )
        );
    }

    #[test]
    fn unexpected_http_error_plain_message_sanitizes_terminal_control_sequences() {
        let err = UnexpectedHttpError {
            status: 500,
            status_text: Some("Internal Server Error".to_string()),
            body: "\u{1b}]8;;https://evil\u{7}oops\u{1b}]8;;\u{7}".to_string(),
            url: Some("https://example.com/\u{1b}[31musage\u{1b}[0m".to_string()),
            cf_ray: Some("ray-\u{7}123".to_string()),
            request_id: Some("req-\u{1b}[2K456".to_string()),
            identity_authorization_error: None,
            identity_error_code: None,
        };

        let plain = err.plain_message();
        assert!(!plain.contains('\u{1b}'));
        assert!(!plain.contains('\u{7}'));
        assert!(plain.contains("oops"));
        assert!(plain.contains("URL: https://example.com/usage"));
        assert!(plain.contains("CF-Ray: ray-123"));
        assert!(plain.contains("Request ID: req-456"));
    }

    #[test]
    fn unexpected_http_error_uses_x_oai_request_id_fallback() {
        let response = http_response(
            "400 Bad Request",
            &[("x-oai-request-id", "req-fallback")],
            "plain body",
        );
        let url = spawn_server(response);
        let response = fetch_response(&url);

        let err = UnexpectedHttpError::from_ureq_response(response, Some(&url));
        let rendered = err.to_string();
        assert!(rendered.contains("request id: req-fallback"));
    }

    #[test]
    fn unexpected_http_error_surfaces_auth_debug_headers() {
        let error_json = STANDARD.encode(r#"{"error":{"code":"deactivated_workspace"}}"#);
        let response = http_response(
            "401 Unauthorized",
            &[
                ("x-openai-authorization-error", "expired session"),
                ("x-error-json", &error_json),
            ],
            "plain body",
        );
        let url = spawn_server(response);
        let response = fetch_response(&url);

        let err = UnexpectedHttpError::from_ureq_response(response, Some(&url));
        let rendered = err.to_string();
        assert!(rendered.contains("auth error: expired session"));
        assert!(rendered.contains("auth error code: deactivated_workspace"));
    }

    #[test]
    fn unexpected_http_error_plain_message_prefers_detail_message() {
        let err = UnexpectedHttpError {
            status: 402,
            status_text: Some("Payment Required".to_string()),
            body: r#"{"detail":{"message":"Workspace deactivated by owner"}}"#.to_string(),
            url: None,
            cf_ray: None,
            request_id: None,
            identity_authorization_error: None,
            identity_error_code: None,
        };

        assert_eq!(
            err.plain_message(),
            "Workspace deactivated by owner\nunexpected status 402 Payment Required"
        );
    }

    #[test]
    fn unexpected_http_error_uses_error_message_for_display() {
        let err = UnexpectedHttpError {
            status: 400,
            status_text: Some("Bad Request".to_string()),
            body: r#"{"error":{"message":"invalid request"}}"#.to_string(),
            url: None,
            cf_ray: None,
            request_id: None,
            identity_authorization_error: None,
            identity_error_code: None,
        };

        assert_eq!(
            err.to_string(),
            "unexpected status 400 Bad Request: invalid request"
        );
        assert!(err.plain_message().starts_with("invalid request\n"));
    }

    #[test]
    fn unexpected_http_error_handles_unknown_status_and_empty_error_message() {
        let err = UnexpectedHttpError {
            status: 599,
            status_text: None,
            body: r#"{"error":{"message":"   "}}"#.to_string(),
            url: None,
            cf_ray: None,
            request_id: None,
            identity_authorization_error: None,
            identity_error_code: None,
        };

        let plain = err.plain_message();
        assert!(plain.starts_with(r#"{"error":{"message":"   "}}"#));
        assert!(plain.contains("unexpected status 599"));
    }

    #[test]
    fn unexpected_http_error_plain_message_includes_auth_debug_context() {
        let err = UnexpectedHttpError {
            status: 401,
            status_text: Some("Unauthorized".to_string()),
            body: "unauthorized".to_string(),
            url: None,
            cf_ray: None,
            request_id: None,
            identity_authorization_error: Some("expired".to_string()),
            identity_error_code: Some("session_expired".to_string()),
        };

        assert_eq!(
            err.plain_message(),
            concat!(
                "unauthorized\n",
                "unexpected status 401 Unauthorized\n",
                "Auth Error: expired\n",
                "Auth Error Code: session_expired"
            )
        );
    }

    #[test]
    fn truncation_preserves_utf8_boundaries() {
        assert_eq!(truncate_with_ellipsis("éclair", 1), "...");
        assert_eq!(truncate_with_ellipsis("éclair", 2), "é...");
    }

    #[test]
    fn terminal_sanitizer_handles_truncated_and_st_terminated_sequences() {
        assert_eq!(sanitize_for_terminal("before\u{1b}"), "before");
        assert_eq!(sanitize_for_terminal("\u{1b}]title\u{1b}\\after"), "after");
        assert_eq!(sanitize_for_terminal("\u{1b}Xafter"), "after");
    }

    #[test]
    fn unexpected_http_error_truncates_long_raw_body() {
        let body = "x".repeat(1205);
        let err = UnexpectedHttpError {
            status: 500,
            status_text: Some("Internal Server Error".to_string()),
            body,
            url: None,
            cf_ray: None,
            request_id: None,
            identity_authorization_error: None,
            identity_error_code: None,
        };

        let rendered = err.to_string();
        assert!(rendered.starts_with("unexpected status 500 Internal Server Error: "));
        assert!(rendered.ends_with("..."));
        assert!(rendered.len() < 1100);
    }

    #[test]
    fn unexpected_http_error_uses_non_json_body_verbatim() {
        let response = http_response("502 Bad Gateway", &[], "gateway exploded");
        let url = spawn_server(response);
        let response = fetch_response(&url);

        let err = UnexpectedHttpError::from_ureq_response(response, Some(&url));
        let rendered = err.to_string();
        assert!(rendered.contains("unexpected status 502 Bad Gateway: gateway exploded"));
    }

    #[test]
    fn resolve_home_dir_uses_userprofile() {
        let out = resolve_home_dir_with(
            None,
            None,
            None,
            Some(PathBuf::from("/tmp/user")),
            None,
            None,
        )
        .unwrap();
        assert_eq!(out, PathBuf::from("/tmp/user"));
    }

    #[cfg(windows)]
    #[test]
    fn resolve_home_dir_uses_drive() {
        let out = resolve_home_dir_with(
            None,
            None,
            None,
            None,
            Some(PathBuf::from("C:")),
            Some(PathBuf::from("\\Users")),
        )
        .unwrap();
        assert_eq!(out, PathBuf::from("C:/Users"));
    }

    #[test]
    fn resolve_home_dir_none_when_empty() {
        assert!(resolve_home_dir_with(None, None, None, None, None, None).is_none());
    }

    #[test]
    fn resolve_home_dir_ignores_empty_values() {
        assert!(
            resolve_home_dir_with(None, None, Some(PathBuf::from("")), None, None, None,).is_none()
        );
        assert!(
            resolve_home_dir_with(None, None, None, Some(PathBuf::from("")), None, None,).is_none()
        );
        assert!(
            resolve_home_dir_with(
                None,
                None,
                None,
                None,
                Some(PathBuf::from("")),
                Some(PathBuf::from("")),
            )
            .is_none()
        );
    }

    #[test]
    fn ensure_paths_errors_when_profiles_is_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profiles = dir.path().join("profiles");
        fs::write(&profiles, "not a dir").expect("write");
        let paths = make_paths(dir.path());
        let err = ensure_paths(&paths).unwrap_err();
        assert!(err.contains("not a directory"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_paths_errors_when_unwritable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let locked = dir.path().join("locked");
        fs::create_dir_all(&locked).expect("create");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o400)).expect("chmod");
        let profiles = locked.join("profiles");
        let mut paths = make_paths(dir.path());
        paths.profiles = profiles.clone();
        paths.profiles_index = profiles.join("profiles.json");
        paths.profiles_lock = profiles.join("profiles.lock");
        let err = ensure_paths(&paths).unwrap_err();
        assert!(err.contains("Cannot create profiles directory"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_paths_permissions_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        with_failpoint(FAIL_SET_PERMISSIONS, || {
            let err = ensure_paths(&paths).unwrap_err();
            assert!(err.contains("Cannot set permissions"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn ensure_paths_profiles_lock_open_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let profiles = dir.path().join("profiles");
        fs::create_dir_all(&profiles).expect("create");
        let lock = profiles.join("profiles.lock");
        fs::write(&lock, "").expect("write lock");
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o400)).expect("chmod");
        let mut paths = make_paths(dir.path());
        paths.profiles_lock = lock.clone();
        let err = ensure_paths(&paths).unwrap_err();
        assert!(err.contains("Cannot write profiles lock file"));
    }

    #[test]
    fn write_atomic_success() {
        with_failpoint_disabled(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("file.txt");
            write_atomic(&path, b"hello").unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
            write_atomic(&path, b"replacement").unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");

            let nested = dir.path().join("nested").join("file.txt");
            write_atomic(&nested, b"nested").unwrap();
            assert_eq!(fs::read_to_string(nested).unwrap(), "nested");
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
        });
    }

    #[test]
    fn write_atomic_invalid_parent() {
        let err = write_atomic(Path::new(""), b"hi").unwrap_err();
        assert!(err.contains("parent directory"));
    }

    #[test]
    fn write_atomic_invalid_filename() {
        let err = write_atomic(Path::new("/"), b"hi").unwrap_err();
        assert!(err.contains("invalid file name") || err.contains("parent directory"));
        let err = write_atomic_private(Path::new("/"), b"hi").unwrap_err();
        assert!(err.contains("invalid file name") || err.contains("parent directory"));
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_rejects_non_utf8_filename() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(OsString::from_vec(vec![0xff]));
        let error = write_atomic(&path, b"credentials").unwrap_err();
        assert!(error.contains("Invalid file name"));
    }

    #[test]
    fn temporary_file_timestamp_rejects_a_pre_epoch_clock() {
        let before_epoch = UNIX_EPOCH
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap();
        let err = temporary_file_timestamp(before_epoch).unwrap_err();
        assert!(err.contains("temporary file timestamp"));
    }

    #[test]
    fn write_atomic_create_dir_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "file").expect("write");
        let path = blocker.join("child.txt");
        let err = write_atomic(&path, b"data").unwrap_err();
        assert!(err.contains("Cannot create directory"));
    }

    #[test]
    fn write_atomic_open_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.txt");
        with_failpoint(FAIL_WRITE_OPEN, || {
            let err = write_atomic(&path, b"data").unwrap_err();
            assert!(err.contains("Failed to create temp file"));
        });
    }

    #[test]
    fn failpoint_guard_can_target_a_later_write_and_restores_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        {
            let _guard = FailpointGuard::new(FAIL_WRITE_WRITE, 2);
            write_atomic(&path, b"first").unwrap();
            assert!(write_atomic(&path, b"second").is_err());
        }
        write_atomic(&path, b"after").unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "after");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_reports_repeated_temp_open_failure() {
        // A maximum-length target name is valid, but the generated temporary
        // name adds a suffix and therefore exceeds the Unix component limit.
        // This exercises bounded retries for create_new failures without
        // relying on Linux-only procfs paths or directory permissions.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x".repeat(255));
        let err = write_atomic(&path, b"data").unwrap_err();
        assert!(err.contains("Failed to create temp file"), "{err}");
    }

    #[test]
    fn write_atomic_write_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.txt");
        with_failpoint(FAIL_WRITE_WRITE, || {
            let err = write_atomic(&path, b"data").unwrap_err();
            assert!(err.contains("Failed to write temp file"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_permissions_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.txt");
        with_failpoint(FAIL_WRITE_PERMS, || {
            let err = write_atomic_with_mode(&path, b"data", 0o600).unwrap_err();
            assert!(err.contains("Failed to set temp file permissions"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_with_mode_creates_private_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.txt");

        write_atomic_with_mode(&path, b"data", 0o600).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn write_atomic_sync_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.txt");
        with_failpoint(FAIL_WRITE_SYNC, || {
            let err = write_atomic(&path, b"data").unwrap_err();
            assert!(err.contains("Failed to write temp file"));
        });
    }

    #[test]
    fn write_atomic_rename_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.txt");
        with_failpoint(FAIL_WRITE_RENAME, || {
            let err = write_atomic(&path, b"data").unwrap_err();
            assert!(err.contains("Failed to replace"));
        });
    }

    #[test]
    fn failed_atomic_writes_preserve_original_and_remove_temporary_credentials() {
        for step in [
            FAIL_WRITE_OPEN,
            FAIL_WRITE_WRITE,
            FAIL_WRITE_PERMS,
            FAIL_WRITE_SYNC,
            FAIL_WRITE_RENAME,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auth.json");
            fs::write(&path, b"original credentials").unwrap();
            with_failpoint(step, || {
                assert!(write_atomic(&path, b"replacement credentials").is_err());
            });
            assert_eq!(fs::read(&path).unwrap(), b"original credentials");
            let paths: Vec<_> = fs::read_dir(dir.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            assert_eq!(
                paths,
                vec![path],
                "temporary credentials left after step {step}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn copying_credentials_tightens_permissions_before_publishing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.json");
        let target = dir.path().join("auth.json");
        fs::write(&source, b"synthetic credentials").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        copy_atomic(&source, &target).unwrap();
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Failure to set private permissions must leave the old destination in place.
        fs::write(&source, b"replacement credentials").unwrap();
        with_failpoint(FAIL_WRITE_PERMS, || {
            assert!(copy_atomic(&source, &target).is_err());
        });
        assert_eq!(fs::read(&target).unwrap(), b"synthetic credentials");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn copy_atomic_reads_source() {
        with_failpoint_disabled(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let source = dir.path().join("source.txt");
            let dest = dir.path().join("dest.txt");
            fs::write(&source, "copy").expect("write");
            copy_atomic(&source, &dest).unwrap();
            assert_eq!(fs::read_to_string(&dest).unwrap(), "copy");
        });
    }

    #[test]
    fn copy_atomic_reports_source_read_failure_after_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source-directory");
        let dest = dir.path().join("dest.txt");
        fs::create_dir(&source).unwrap();
        let err = copy_atomic(&source, &dest).unwrap_err();
        assert!(err.contains("Failed to read"));
    }

    #[test]
    fn copy_atomic_missing_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("missing.txt");
        let dest = dir.path().join("dest.txt");
        let err = copy_atomic(&source, &dest).unwrap_err();
        assert!(err.contains("Failed to read metadata"));
    }

    #[test]
    fn ensure_file_or_absent_errors_on_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = ensure_file_or_absent(dir.path()).unwrap_err();
        assert!(err.contains("exists and is not a file"));
    }
}
