use crate::{Paths, is_plain, set_plain};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::cell::Cell;
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::thread;

pub(crate) static ENV_MUTEX: Mutex<()> = Mutex::new(());
pub(crate) static PLAIN_MUTEX: Mutex<()> = Mutex::new(());

thread_local! {
    static PLAIN_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub(crate) struct EnvVarGuard {
    key: String,
    prev: Option<String>,
}

pub(crate) struct PlainGuard {
    prev: bool,
    _lock: Option<MutexGuard<'static, ()>>,
}

fn set_env(key: &str, value: Option<&str>) -> Option<String> {
    let prev = env::var(key).ok();
    if let Some(value) = value {
        // SAFETY: ENV_MUTEX is held by the caller (via EnvVarGuard / set_env_guard),
        // serializing all env var mutations across test threads.
        unsafe {
            env::set_var(key, value);
        }
    } else {
        // SAFETY: Same as above — ENV_MUTEX serializes access.
        unsafe {
            env::remove_var(key);
        }
    }
    prev
}

pub(crate) fn set_env_guard(key: &str, value: Option<&str>) -> EnvVarGuard {
    EnvVarGuard {
        key: key.to_string(),
        prev: set_env(key, value),
    }
}

pub(crate) fn set_plain_guard(value: bool) -> PlainGuard {
    let lock = PLAIN_DEPTH.with(|depth| {
        let current = depth.get();
        depth.set(current + 1);
        if current == 0 {
            Some(PLAIN_MUTEX.lock().unwrap())
        } else {
            None
        }
    });
    let prev = is_plain();
    set_plain(value);
    PlainGuard { prev, _lock: lock }
}

fn restore_env(key: &str, prev: Option<String>) {
    if let Some(value) = prev {
        // SAFETY: ENV_MUTEX is held via EnvVarGuard's Drop impl, serializing env var mutations.
        unsafe {
            env::set_var(key, value);
        }
    } else {
        // SAFETY: Same as above — ENV_MUTEX serializes access.
        unsafe {
            env::remove_var(key);
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        restore_env(&self.key, prev);
    }
}

impl Drop for PlainGuard {
    fn drop(&mut self) {
        set_plain(self.prev);
        PLAIN_DEPTH.with(|depth| {
            let current = depth.get();
            depth.set(current.saturating_sub(1));
        });
    }
}

pub(crate) fn spawn_server(response: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            // Consume the complete request before closing the socket. Windows
            // resets a connection closed with unread request-body bytes.
            let mut reader = BufReader::new(&mut stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://{}", addr)
}

pub(crate) fn build_id_token(email: &str, plan: &str) -> String {
    let header = serde_json::json!({
        "alg": "none",
        "typ": "JWT",
    });
    let auth = serde_json::json!({
        "chatgpt_plan_type": plan,
    });
    let payload = serde_json::json!({
        "email": email,
        "https://api.openai.com/auth": auth,
    });
    let header = URL_SAFE_NO_PAD.encode(serde_json::to_string(&header).unwrap());
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_string(&payload).unwrap());
    format!("{header}.{payload}.")
}

pub(crate) fn http_ok_response(body: &str, content_type: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len(),
    )
}

pub(crate) fn make_paths(root: &Path) -> Paths {
    let codex = root.to_path_buf();
    let auth = codex.join("auth.json");
    let profiles = codex.join("profiles");
    let profiles_index = profiles.join("profiles.json");
    let update_cache = profiles.join("update.json");
    let profiles_lock = profiles.join("profiles.lock");
    Paths {
        codex,
        auth,
        profiles,
        profiles_index,
        update_cache,
        profiles_lock,
    }
}
