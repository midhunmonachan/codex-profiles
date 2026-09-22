use chrono::{DateTime, Local, TimeZone, Utc};
use colored::Colorize;
use fslock::LockFile;
use serde::{Deserialize, Serialize};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{
    Paths, UI_INFO_PREFIX, USAGE_ERR_INVALID_RESPONSE, USAGE_ERR_LOCK_ACQUIRE, USAGE_ERR_LOCK_HELD,
    USAGE_ERR_LOCK_OPEN, USAGE_ERR_SERVICE_UNREACHABLE, USAGE_UNAVAILABLE_DEFAULT, command_name,
};
use crate::{is_plain, style_text, use_color_stdout};

const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const USER_AGENT: &str = "codex-profiles";
const USAGE_RETRY_ATTEMPTS: usize = 3;
const USAGE_RETRY_BASE_MS: u64 = 250;
const USAGE_BACKOFF_MAX_MS: u64 = 3_000;
const USAGE_RETRY_JITTER_MS: u64 = 125;
const LOCK_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(10)
};
const LOCK_RETRY_DELAY: Duration = Duration::from_secs(1);

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
const LOCK_FAIL_ERR: usize = 1;
#[cfg(test)]
const LOCK_FAIL_BUSY: usize = 2;
#[cfg(test)]
thread_local! {
    static LOCK_FAILPOINT: Cell<usize> = const { Cell::new(0) };
}

#[derive(Clone, Default)]
pub(crate) struct UsageLimits {
    pub(crate) primary: Option<UsageWindow>,
    pub(crate) secondary: Option<UsageWindow>,
}

#[derive(Clone, Debug)]
pub(crate) struct UsageWindow {
    pub(crate) left_percent: f64,
    pub(crate) reset_at: i64,
    pub(crate) window_seconds: i64,
}

#[derive(Clone, Serialize)]
pub(crate) struct UsageSnapshotWindow {
    pub(crate) left_percent: i64,
    pub(crate) reset_at: i64,
    pub(crate) window_seconds: i64,
}

#[derive(Clone, Serialize)]
pub(crate) struct UsageSnapshotBucket {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) primary: Option<UsageSnapshotWindow>,
    pub(crate) secondary: Option<UsageSnapshotWindow>,
    // Compatibility aliases, populated only for the durations they name.
    pub(crate) five_hour: Option<UsageSnapshotWindow>,
    pub(crate) weekly: Option<UsageSnapshotWindow>,
}

#[derive(Debug)]
pub enum UsageFetchError {
    Http(Box<crate::UnexpectedHttpError>),
    Transport(String),
    Parse(String),
}

impl UsageFetchError {
    pub fn status_code(&self) -> Option<u16> {
        match self {
            UsageFetchError::Http(err) => Some(err.status_code()),
            _ => None,
        }
    }

    pub fn message(&self) -> String {
        match self {
            UsageFetchError::Http(err) => err.to_string(),
            UsageFetchError::Transport(err) => crate::msg1(USAGE_ERR_SERVICE_UNREACHABLE, err),
            UsageFetchError::Parse(err) => crate::msg1(USAGE_ERR_INVALID_RESPONSE, err),
        }
    }

    pub fn plain_message(&self) -> String {
        match self {
            UsageFetchError::Http(err) => err.plain_message(),
            UsageFetchError::Transport(err) => crate::msg1(USAGE_ERR_SERVICE_UNREACHABLE, err),
            UsageFetchError::Parse(err) => crate::msg1(USAGE_ERR_INVALID_RESPONSE, err),
        }
    }
}

impl std::fmt::Display for UsageFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

#[derive(Debug, Deserialize)]
struct UsagePayload {
    #[serde(default)]
    rate_limit: Option<RateLimitDetails>,
    #[serde(default)]
    additional_rate_limits: Option<Vec<AdditionalRateLimitDetails>>,
}

#[derive(Clone, Debug, Deserialize)]
struct RateLimitDetails {
    #[serde(default)]
    primary_window: Option<RateLimitWindowSnapshot>,
    #[serde(default)]
    secondary_window: Option<RateLimitWindowSnapshot>,
}

#[derive(Clone, Debug, Deserialize)]
struct AdditionalRateLimitDetails {
    #[serde(default)]
    limit_name: Option<String>,
    #[serde(default)]
    metered_feature: Option<String>,
    #[serde(default)]
    rate_limit: Option<RateLimitDetails>,
}

#[derive(Clone, Debug, Deserialize)]
struct RateLimitWindowSnapshot {
    used_percent: f64,
    limit_window_seconds: i64,
    reset_at: i64,
}

#[derive(Clone, Debug)]
struct UsageBucket {
    limit_id: String,
    label: String,
    rate_limit: Option<RateLimitDetails>,
}

pub fn read_base_url(paths: &Paths) -> Result<String, String> {
    let config_path = paths.codex.join("config.toml");
    match crate::common::read_config_string(&config_path, &["chatgpt_base_url"])? {
        Some(value) => validate_base_url(&value),
        None => Ok(DEFAULT_BASE_URL.to_string()),
    }
}
fn normalize_base_url(value: &str) -> String {
    let mut base = value.trim_end_matches('/').to_string();
    if let Some((scheme, host)) = parsed_url_scheme_and_host(&base)
        && scheme == "https"
        && matches!(host.as_str(), "chatgpt.com" | "chat.openai.com")
        && !base.contains("/backend-api")
    {
        base = format!("{base}/backend-api");
    }
    base
}

fn validate_base_url(value: &str) -> Result<String, String> {
    let base = normalize_base_url(value);
    if is_allowed_base_url(&base) {
        return Ok(base);
    }
    // Configuration can contain query credentials or other private routing
    // data. Report the setting name without reflecting its value.
    Err(
        "Unsupported chatgpt_base_url. Use an official ChatGPT host or a loopback address."
            .to_string(),
    )
}

fn is_allowed_base_url(base_url: &str) -> bool {
    let Some((scheme, host)) = parsed_url_scheme_and_host(base_url) else {
        return false;
    };
    if is_loopback_host(&host) {
        return matches!(scheme.as_str(), "http" | "https");
    }
    scheme == "https" && matches!(host.as_str(), "chatgpt.com" | "chat.openai.com")
}

pub(crate) fn parsed_url_scheme_and_host(base_url: &str) -> Option<(String, String)> {
    let (scheme, rest) = base_url
        .split_once("://")
        .map(|(scheme, rest)| (scheme.to_ascii_lowercase(), rest))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() || authority.contains('@') {
        return None;
    }

    let host = if authority.starts_with('[') {
        authority
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or(authority)
            .to_ascii_lowercase()
    } else {
        authority
            .split(':')
            .next()
            .unwrap_or(authority)
            .trim_end_matches('.')
            .to_ascii_lowercase()
    };

    if host.is_empty() {
        return None;
    }

    Some((scheme, host))
}

pub(crate) fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .ok()
            .is_some_and(|ip| ip.is_loopback())
        || is_ipv4_loopback_shorthand(host)
}

fn is_ipv4_loopback_shorthand(host: &str) -> bool {
    let mut parts = host.split('.');
    if parts.next() != Some("127") {
        return false;
    }

    let mut count = 1usize;
    for part in parts {
        if part.is_empty() || part.parse::<u8>().is_err() {
            return false;
        }
        count += 1;
    }

    count >= 2
}

fn usage_endpoint(base_url: &str) -> String {
    if base_url.contains("/backend-api") {
        format!("{base_url}/wham/usage")
    } else {
        format!("{base_url}/api/codex/usage")
    }
}

fn fetch_usage_payload(
    base_url: &str,
    access_token: &str,
    account_id: &str,
) -> Result<UsagePayload, UsageFetchError> {
    let endpoint = usage_endpoint(base_url);
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .build();
    let agent: ureq::Agent = config.into();
    let mut attempt = 0;
    loop {
        let response = match agent
            .get(&endpoint)
            .header("Authorization", &format!("Bearer {access_token}"))
            .header("ChatGPT-Account-Id", account_id)
            .header("User-Agent", USER_AGENT)
            .call()
        {
            Ok(response) => response,
            Err(err) => {
                if usage_should_retry_transport_error(&err)
                    && let Some(delay) = usage_retry_delay(attempt, None)
                {
                    thread::sleep(delay);
                    attempt += 1;
                    continue;
                }
                return Err(UsageFetchError::Transport(err.to_string()));
            }
        };
        let status = response.status();
        if usage_should_retry_status(status.as_u16()) {
            let retry_after = response
                .headers()
                .get("Retry-After")
                .and_then(|value| value.to_str().ok());
            if let Some(delay) = usage_retry_delay(attempt, retry_after) {
                thread::sleep(delay);
                attempt += 1;
                continue;
            }
        }
        if !status.is_success() {
            return Err(UsageFetchError::Http(Box::new(
                crate::UnexpectedHttpError::from_ureq_response(response, Some(&endpoint)),
            )));
        }
        return response
            .into_body()
            .read_json::<UsagePayload>()
            .map_err(|err| UsageFetchError::Parse(err.to_string()));
    }
}

fn usage_should_retry_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

fn usage_should_retry_transport_error(err: &ureq::Error) -> bool {
    matches!(
        err,
        ureq::Error::Timeout(_)
            | ureq::Error::Io(_)
            | ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
    )
}

fn usage_retry_delay(attempt: usize, retry_after: Option<&str>) -> Option<Duration> {
    if attempt + 1 >= USAGE_RETRY_ATTEMPTS {
        return None;
    }
    if let Some(delay) = retry_after.and_then(parse_retry_after) {
        // Honor long server delays by returning the response to the caller.
        // Never retry earlier than requested or block an interactive command
        // for an unbounded Retry-After value.
        return (delay <= Duration::from_millis(USAGE_BACKOFF_MAX_MS)).then_some(delay);
    }
    let shift = attempt.min(10) as u32;
    let base = USAGE_RETRY_BASE_MS.saturating_mul(1u64 << shift);
    let mut delay = Duration::from_millis(base.min(USAGE_BACKOFF_MAX_MS));
    let jitter = usage_retry_jitter();
    delay += jitter;
    Some(delay.min(Duration::from_millis(USAGE_BACKOFF_MAX_MS)))
}

fn usage_retry_jitter() -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    Duration::from_millis(nanos % (USAGE_RETRY_JITTER_MS + 1))
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let parsed = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let retry_at = parsed.with_timezone(&Utc).timestamp();
    let now = Utc::now().timestamp();
    if retry_at <= now {
        return Some(Duration::from_millis(0));
    }
    Some(Duration::from_secs((retry_at - now) as u64))
}

pub(crate) fn fetch_usage_status(
    base_url: &str,
    access_token: &str,
    account_id: &str,
    unavailable_text: &str,
    now: DateTime<Local>,
) -> Result<(Vec<String>, Vec<UsageSnapshotBucket>), UsageFetchError> {
    let payload = fetch_usage_payload(base_url, access_token, account_id)?;
    Ok((
        usage_lines_from_payload(&payload, unavailable_text, now),
        usage_snapshot_from_payload(&payload),
    ))
}

#[cfg(test)]
fn build_usage_limits(payload: &UsagePayload) -> UsageLimits {
    let buckets = ordered_usage_buckets(usage_buckets(payload));
    let Some(preferred_bucket) = buckets.first() else {
        return UsageLimits::default();
    };
    build_usage_limits_for_rate_limit(preferred_bucket.rate_limit.as_ref())
}

fn usage_lines_from_payload(
    payload: &UsagePayload,
    unavailable_text: &str,
    now: DateTime<Local>,
) -> Vec<String> {
    let buckets = ordered_usage_buckets(usage_buckets(payload));
    if buckets.is_empty() {
        return vec![format_usage_unavailable(
            unavailable_text,
            use_color_stdout(),
        )];
    }
    let multi_bucket = buckets.len() > 1;
    let mut lines = Vec::new();
    for bucket in buckets {
        let limits = build_usage_limits_for_rate_limit(bucket.rate_limit.as_ref());
        let has_data = limits.primary.is_some() || limits.secondary.is_some();
        if !has_data {
            continue;
        }
        let bucket_lines = format_usage(
            format_limit(limits.primary.as_ref(), now, unavailable_text),
            format_limit(limits.secondary.as_ref(), now, unavailable_text),
            unavailable_text,
        );
        let bucket_lines = [limits.primary.as_ref(), limits.secondary.as_ref()]
            .into_iter()
            .enumerate()
            .filter_map(|(index, window)| window.map(|window| (index, window)))
            .zip(bucket_lines)
            .map(|((index, window), line)| {
                format!(
                    "{}: {line}",
                    usage_window_label(window.window_seconds, index == 1)
                )
            });
        if multi_bucket {
            let label = usage_bucket_label(&bucket);
            lines.push(label.to_string());
            lines.extend(bucket_lines.into_iter().map(|line| format!("  {line}")));
        } else {
            lines.extend(bucket_lines);
        }
    }
    if lines.is_empty() {
        vec![format_usage_unavailable(
            unavailable_text,
            use_color_stdout(),
        )]
    } else {
        lines
    }
}

fn usage_window_label(seconds: i64, is_secondary: bool) -> String {
    match seconds {
        604_800 => "Weekly".to_string(),
        2_592_000 => "Monthly".to_string(),
        value if value > 0 && value % 86_400 == 0 => format!("{} day", value / 86_400),
        value if value > 0 && value % 3_600 == 0 => format!("{} hour", value / 3_600),
        value if value > 0 && value % 60 == 0 => format!("{} minute", value / 60),
        value if value > 0 => format!("{value} second"),
        _ if is_secondary => "Secondary".to_string(),
        _ => "Primary".to_string(),
    }
}

fn usage_buckets(payload: &UsagePayload) -> Vec<UsageBucket> {
    let mut buckets = Vec::new();
    if let Some(rate_limit) = payload.rate_limit.clone() {
        buckets.push(UsageBucket {
            limit_id: "codex".to_string(),
            label: "codex".to_string(),
            rate_limit: Some(rate_limit),
        });
    }
    if let Some(additional) = payload.additional_rate_limits.as_ref() {
        buckets.extend(additional.iter().map(|details| {
            let limit_id = details
                .metered_feature
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("unknown")
                .to_string();
            let label = details
                .limit_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(limit_id.as_str())
                .to_string();
            UsageBucket {
                limit_id,
                label,
                rate_limit: details.rate_limit.clone(),
            }
        }));
    }
    buckets
}

fn ordered_usage_buckets(mut buckets: Vec<UsageBucket>) -> Vec<UsageBucket> {
    if let Some(index) = buckets.iter().position(|bucket| bucket.limit_id == "codex")
        && index != 0
    {
        let preferred = buckets.remove(index);
        buckets.insert(0, preferred);
    }
    buckets
}

fn usage_bucket_label(bucket: &UsageBucket) -> &str {
    if bucket.label.trim().is_empty() {
        "unknown"
    } else {
        bucket.label.as_str()
    }
}

fn build_usage_limits_for_rate_limit(rate_limit: Option<&RateLimitDetails>) -> UsageLimits {
    let Some(rate_limit) = rate_limit else {
        return UsageLimits::default();
    };
    UsageLimits {
        primary: rate_limit.primary_window.as_ref().map(usage_window_output),
        secondary: rate_limit
            .secondary_window
            .as_ref()
            .map(usage_window_output),
    }
}

fn usage_snapshot_from_payload(payload: &UsagePayload) -> Vec<UsageSnapshotBucket> {
    ordered_usage_buckets(usage_buckets(payload))
        .into_iter()
        .filter_map(|bucket| {
            let limits = build_usage_limits_for_rate_limit(bucket.rate_limit.as_ref());
            let primary = limits.primary.as_ref().map(usage_snapshot_window);
            let secondary = limits.secondary.as_ref().map(usage_snapshot_window);
            if primary.is_none() && secondary.is_none() {
                return None;
            }
            let window_for_duration = |seconds| {
                [limits.primary.as_ref(), limits.secondary.as_ref()]
                    .into_iter()
                    .flatten()
                    .find(|window| window.window_seconds == seconds)
                    .map(usage_snapshot_window)
            };
            let label = usage_bucket_label(&bucket).to_string();
            Some(UsageSnapshotBucket {
                id: bucket.limit_id,
                label,
                primary,
                secondary,
                five_hour: window_for_duration(18_000),
                weekly: window_for_duration(604_800),
            })
        })
        .collect()
}

fn usage_snapshot_window(window: &UsageWindow) -> UsageSnapshotWindow {
    UsageSnapshotWindow {
        left_percent: window.left_percent.round() as i64,
        reset_at: window.reset_at,
        window_seconds: window.window_seconds,
    }
}

fn usage_window_output(window: &RateLimitWindowSnapshot) -> UsageWindow {
    let left_percent = (100.0 - window.used_percent).clamp(0.0, 100.0);
    let reset_at = window.reset_at;
    UsageWindow {
        left_percent,
        reset_at,
        window_seconds: window.limit_window_seconds,
    }
}

pub(crate) struct UsageLine {
    pub(crate) bar: String,
    pub(crate) percent: String,
    pub(crate) reset: String,
    pub(crate) left_percent: Option<i64>,
}

impl UsageLine {
    fn unavailable(text: &str) -> Self {
        UsageLine {
            bar: text.to_string(),
            percent: String::new(),
            reset: String::new(),
            left_percent: None,
        }
    }
}

pub(crate) fn format_limit(
    window: Option<&UsageWindow>,
    now: DateTime<Local>,
    unavailable_text: &str,
) -> UsageLine {
    let Some(window) = window else {
        return UsageLine::unavailable(unavailable_text);
    };
    let left_percent = window.left_percent;
    let left_percent_rounded = left_percent.round() as i64;
    let bar = render_bar(left_percent);
    let bar = style_usage_bar(&bar, left_percent);
    let percent = format!("{left_percent_rounded}%");
    let reset =
        format_reset_timestamp(window.reset_at, now).unwrap_or_else(|| "unknown".to_string());
    UsageLine {
        bar,
        percent,
        reset,
        left_percent: Some(left_percent_rounded),
    }
}

pub fn usage_unavailable() -> &'static str {
    USAGE_UNAVAILABLE_DEFAULT
}

pub fn format_usage_unavailable(text: &str, use_color: bool) -> String {
    if is_plain() {
        crate::msg1(UI_INFO_PREFIX, text)
    } else if use_color {
        text.red().bold().to_string()
    } else {
        text.to_string()
    }
}

pub(crate) fn format_usage(
    primary: UsageLine,
    secondary: UsageLine,
    unavailable_text: &str,
) -> Vec<String> {
    let use_color = use_color_stdout();
    let available: Vec<UsageLine> = [primary, secondary]
        .into_iter()
        .filter(|line| line.left_percent.is_some())
        .collect();
    if available.is_empty() {
        return vec![format_usage_unavailable(unavailable_text, use_color)];
    }
    let has_zero = available.iter().any(|line| line.left_percent == Some(0));
    let multiple = available.len() > 1;
    available
        .into_iter()
        .map(|line| {
            let dim = use_color && multiple && has_zero && line.left_percent != Some(0);
            format_usage_line(&line, dim, use_color)
        })
        .collect()
}

pub(crate) fn format_reset_timestamp(reset_at: i64, now: DateTime<Local>) -> Option<String> {
    let reset_at = local_from_timestamp(reset_at)?;
    let time = reset_at.format("%H:%M").to_string();
    if reset_at.date_naive() == now.date_naive() {
        Some(time)
    } else {
        Some(format!("{time} on {}", reset_at.format("%-d %b")))
    }
}

fn format_usage_line(line: &UsageLine, dim: bool, use_color: bool) -> String {
    let reset = reset_label(&line.reset);
    let reset = reset.to_string();
    let percent = if line.percent.is_empty() {
        String::new()
    } else {
        format!("{} left", line.percent)
    };
    let resets = format_resets_suffix(&reset, use_color);
    if is_plain() {
        let mut out = String::new();
        if !percent.is_empty() {
            out.push_str(&percent);
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&resets);
        return out;
    }
    // `format_resets_suffix` always returns the parenthesized reset label, so
    // the non-empty branch is the only reachable representation here.
    let resets = format!(" {resets}");
    let bar = if dim {
        crate::ui::strip_ansi(&line.bar)
    } else {
        line.bar.clone()
    };
    let formatted = if percent.is_empty() {
        format!("{bar}{resets}")
    } else {
        format!("{bar} {percent}{resets}")
    };
    if dim && use_color {
        formatted.dimmed().to_string()
    } else {
        formatted
    }
}

fn reset_label(reset: &str) -> &str {
    if reset.is_empty() { "unknown" } else { reset }
}

fn format_resets_suffix(reset: &str, use_color: bool) -> String {
    let text = format!("(resets {reset})");
    style_text(&text, use_color, |text| text.dimmed().italic())
}

fn render_bar(left_percent: f64) -> String {
    let total = 20;
    let filled = ((left_percent / 100.0) * total as f64).round() as usize;
    let filled = filled.min(total);
    let empty = total.saturating_sub(filled);
    format!(
        "{}{}",
        "▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮"
            .chars()
            .take(filled)
            .collect::<String>(),
        "▯▯▯▯▯▯▯▯▯▯▯▯▯▯▯▯▯▯▯▯"
            .chars()
            .take(empty)
            .collect::<String>()
    )
}

fn style_usage_bar(bar: &str, left_percent: f64) -> String {
    style_usage_bar_with_color(bar, left_percent, use_color_stdout())
}

fn style_usage_bar_with_color(bar: &str, left_percent: f64, use_color: bool) -> String {
    if !use_color {
        return bar.to_string();
    }
    if left_percent >= 66.0 {
        bar.green().to_string()
    } else if left_percent >= 33.0 {
        bar.yellow().to_string()
    } else {
        bar.red().to_string()
    }
}

fn local_from_timestamp(ts: i64) -> Option<DateTime<Local>> {
    let dt = chrono::Utc.timestamp_opt(ts, 0).single()?;
    Some(dt.with_timezone(&Local))
}

#[derive(Debug)]
pub struct UsageLock {
    _lock: LockFile,
}

pub fn lock_usage(paths: &Paths) -> Result<UsageLock, String> {
    let start = Instant::now();
    let mut lock = LockFile::open(&paths.profiles_lock)
        .map_err(|err| crate::msg1(USAGE_ERR_LOCK_OPEN, err))?;
    loop {
        match try_lock(&mut lock) {
            Ok(true) => break,
            Ok(false) => {
                if start.elapsed() > LOCK_TIMEOUT {
                    return Err(crate::msg1(USAGE_ERR_LOCK_ACQUIRE, command_name()));
                }
                thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(err) => {
                return Err(crate::msg1(USAGE_ERR_LOCK_HELD, err));
            }
        }
    }
    Ok(UsageLock { _lock: lock })
}

#[cfg(not(test))]
fn try_lock(lock: &mut LockFile) -> Result<bool, fslock::Error> {
    lock.try_lock()
}

#[cfg(test)]
fn try_lock(lock: &mut LockFile) -> Result<bool, fslock::Error> {
    let fail_mode = LOCK_FAILPOINT.with(|failpoint| failpoint.get());
    match fail_mode {
        LOCK_FAIL_ERR => Err(std::io::Error::other("fail")),
        LOCK_FAIL_BUSY => Ok(false),
        _ => lock.try_lock(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{
        http_ok_response, make_paths, set_env_guard, set_plain_guard, spawn_server,
    };
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::thread;

    static LOCK_TEST_MUTEX: Mutex<()> = Mutex::new(());

    enum TestServerStep {
        Close,
        Respond(String),
    }

    fn spawn_server_sequence(steps: Vec<TestServerStep>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        thread::spawn(move || {
            for step in steps {
                let (mut stream, _) = listener.accept().expect("test server accept");
                let mut buf = [0_u8; 4096];
                let _ = stream.read(&mut buf);
                match step {
                    TestServerStep::Close => {}
                    TestServerStep::Respond(response) => {
                        let _ = stream.write_all(response.as_bytes());
                    }
                }
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn base_url_ignores_nested_provider_settings() {
        let dir = tempfile::tempdir().unwrap();
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.codex).unwrap();
        fs::write(
            paths.codex.join("config.toml"),
            "[model_providers.custom]\nchatgpt_base_url = 'http://localhost:9999'\n",
        )
        .unwrap();
        assert_eq!(read_base_url(&paths).unwrap(), DEFAULT_BASE_URL);
        fs::write(
            paths.codex.join("config.toml"),
            "chatgpt_base_url = 'https://chatgpt.com' # comment\n",
        )
        .unwrap();
        assert_eq!(read_base_url(&paths).unwrap(), DEFAULT_BASE_URL);
    }
    #[test]
    fn normalize_base_url_and_endpoint() {
        let url = normalize_base_url("https://chatgpt.com");
        assert!(url.ends_with("/backend-api"));
        assert!(usage_endpoint(&url).contains("wham/usage"));
        assert!(usage_endpoint("http://example.com").contains("api/codex/usage"));
    }

    #[test]
    fn read_base_url_rejects_unsafe_remote_hosts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.codex).unwrap();
        fs::write(
            paths.codex.join("config.toml"),
            "chatgpt_base_url = \"http://example.com\"\n",
        )
        .unwrap();

        let err = read_base_url(&paths).unwrap_err();

        assert!(err.contains("Unsupported chatgpt_base_url"));
    }

    #[test]
    fn rejected_base_url_does_not_echo_configuration_secrets() {
        let error = validate_base_url("https://private.example/secret-path?token=private-token")
            .unwrap_err();
        assert!(error.contains("Unsupported chatgpt_base_url"));
        assert!(!error.contains("private"));
        assert!(!error.contains("secret"));
    }

    #[test]
    fn read_base_url_rejects_spoofed_official_hosts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.codex).unwrap();

        for value in [
            "https://chatgpt.com.evil.test",
            "https://chatgpt.com@evil.test",
        ] {
            fs::write(
                paths.codex.join("config.toml"),
                format!("chatgpt_base_url = \"{value}\"\n"),
            )
            .unwrap();

            let err = read_base_url(&paths).unwrap_err();
            assert!(err.contains("Unsupported chatgpt_base_url"));
        }
    }

    #[test]
    fn read_base_url_allows_loopback_hosts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.codex).unwrap();
        for value in [
            "http://127.0.0.1:8765",
            "http://127.0.0.2:8765",
            "http://127.1:8765",
            "http://localhost:8765",
            "http://[::1]:8765",
        ] {
            fs::write(
                paths.codex.join("config.toml"),
                format!("chatgpt_base_url = \"{value}\"\n"),
            )
            .unwrap();

            let base_url = read_base_url(&paths).unwrap();

            assert_eq!(base_url, value);
        }
    }

    #[test]
    fn read_base_url_rejects_invalid_loopback_shorthand_hosts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.codex).unwrap();
        for value in [
            "http://127..1:8765",
            "http://127.a:8765",
            "http://127.256:8765",
        ] {
            fs::write(
                paths.codex.join("config.toml"),
                format!("chatgpt_base_url = \"{value}\"\n"),
            )
            .unwrap();

            let err = read_base_url(&paths).unwrap_err();
            assert!(err.contains("Unsupported chatgpt_base_url"));
        }
    }

    #[test]
    fn parsed_url_rejects_a_missing_scheme_without_leaking_input() {
        assert_eq!(parsed_url_scheme_and_host("127.0.0.1:8765"), None);
        assert_eq!(parsed_url_scheme_and_host("localhost"), None);
        assert_eq!(parsed_url_scheme_and_host("https:///path"), None);
    }

    #[test]
    fn fetch_usage_payload_paths() {
        let payload = r#"{"rate_limit":{"primary_window":{"used_percent":50.0,"limit_window_seconds":3600,"reset_at":1}}}"#;
        let resp = http_ok_response(payload, "application/json");
        let url = spawn_server(resp);
        let base_url = format!("{url}/backend-api");
        fetch_usage_payload(&base_url, "token", "acct").unwrap();

        let err_body = "server exploded";
        let err_resp = format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\n\r\n{}",
            err_body.len(),
            err_body
        );
        let err_steps = (0..USAGE_RETRY_ATTEMPTS)
            .map(|_| TestServerStep::Respond(err_resp.clone()))
            .collect();
        let err_url = spawn_server_sequence(err_steps);
        let base_url = format!("{err_url}/backend-api");
        let err = fetch_usage_payload(&base_url, "token", "acct").unwrap_err();
        assert!(matches!(err, UsageFetchError::Http(_)));
        assert!(
            err.message()
                .contains("unexpected status 500 Internal Server Error: server exploded")
        );

        let code_body = r#"{"detail":{"code":"deactivated_workspace"}}"#;
        let code_resp = format!(
            "HTTP/1.1 402 Payment Required\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            code_body.len(),
            code_body
        );
        let code_url = spawn_server(code_resp);
        let base_url = format!("{code_url}/backend-api");
        let err = fetch_usage_payload(&base_url, "token", "acct").unwrap_err();
        assert!(err.message().contains("unexpected status 402 Payment Required: {\"detail\":{\"code\":\"deactivated_workspace\"}}"));

        let bad_resp =
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1\r\n\r\n{"
                .to_string();
        let bad_url = spawn_server(bad_resp);
        let base_url = format!("{bad_url}/backend-api");
        let err = fetch_usage_payload(&base_url, "token", "acct").unwrap_err();
        assert!(matches!(err, UsageFetchError::Parse(_)));
    }

    #[test]
    fn fetch_usage_payload_retries_http_5xx_before_success() {
        let ok_payload = r#"{"rate_limit":{"primary_window":{"used_percent":50.0,"limit_window_seconds":3600,"reset_at":1}}}"#;
        let ok_response = http_ok_response(ok_payload, "application/json");
        let error_body = "temporary failure";
        let error_response = format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\n\r\n{}",
            error_body.len(),
            error_body
        );
        let url = spawn_server_sequence(vec![
            TestServerStep::Respond(error_response),
            TestServerStep::Respond(ok_response),
        ]);
        let base_url = format!("{url}/backend-api");

        let payload = fetch_usage_payload(&base_url, "token", "acct").unwrap();
        assert!(payload.rate_limit.is_some());
    }

    #[test]
    fn fetch_usage_payload_retries_transport_errors_before_success() {
        let ok_payload = r#"{"rate_limit":{"primary_window":{"used_percent":50.0,"limit_window_seconds":3600,"reset_at":1}}}"#;
        let ok_response = http_ok_response(ok_payload, "application/json");
        let url = spawn_server_sequence(vec![
            TestServerStep::Close,
            TestServerStep::Respond(ok_response),
        ]);
        let base_url = format!("{url}/backend-api");

        let payload = fetch_usage_payload(&base_url, "token", "acct").unwrap();
        assert!(payload.rate_limit.is_some());
    }

    #[test]
    fn fetch_usage_payload_returns_transport_error_after_retry_budget() {
        let steps = (0..USAGE_RETRY_ATTEMPTS)
            .map(|_| TestServerStep::Close)
            .collect();
        let url = spawn_server_sequence(steps);
        let base_url = format!("{url}/backend-api");

        let err = fetch_usage_payload(&base_url, "token", "acct").unwrap_err();
        assert!(matches!(err, UsageFetchError::Transport(_)));
    }

    #[test]
    fn fetch_usage_details_paths() {
        let payload = r#"{"rate_limit":{"primary_window":{"used_percent":10.0,"limit_window_seconds":3600,"reset_at":1}}}"#;
        let resp = http_ok_response(payload, "application/json");
        let url = spawn_server(resp);
        let base_url = format!("{url}/backend-api");
        let (lines, buckets) =
            fetch_usage_status(&base_url, "token", "acct", "unavailable", Local::now()).unwrap();
        assert!(!lines.is_empty());
        assert!(!buckets.is_empty());
    }

    #[test]
    fn retry_after_parsing_paths() {
        assert_eq!(parse_retry_after("  "), None);
        assert_eq!(parse_retry_after("2"), Some(Duration::from_secs(2)));
        assert!(parse_retry_after("Thu, 01 Jan 1970 00:00:00 GMT").is_some());
        let future = (Utc::now() + chrono::Duration::seconds(60)).to_rfc2822();
        assert!(parse_retry_after(&future).is_some_and(|delay| delay > Duration::ZERO));
        assert!(parse_retry_after("not-a-date").is_none());
        assert!(usage_retry_delay(USAGE_RETRY_ATTEMPTS - 1, Some("1")).is_none());
        assert!(usage_retry_delay(0, Some("2")).is_some());
        assert_eq!(usage_retry_delay(0, Some("7")), None);
        assert_eq!(usage_retry_delay(0, Some("18446744073709551615")), None);
        assert_eq!(
            usage_retry_delay(0, Some("3")),
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn usage_errors_and_transport_retry_classification() {
        let transport = UsageFetchError::Transport("connection closed".to_string());
        assert!(transport.message().contains("connection closed"));
        assert!(transport.plain_message().contains("connection closed"));
        assert_eq!(transport.status_code(), None);
        assert_eq!(transport.to_string(), transport.message());

        let parse = UsageFetchError::Parse("invalid JSON".to_string());
        assert!(parse.message().contains("invalid JSON"));
        assert!(parse.plain_message().contains("invalid JSON"));

        assert!(usage_should_retry_transport_error(&ureq::Error::Timeout(
            ureq::Timeout::Global,
        )));
        assert!(usage_should_retry_transport_error(&ureq::Error::Io(
            std::io::Error::other("socket"),
        )));
        assert!(usage_should_retry_transport_error(
            &ureq::Error::HostNotFound
        ));
        assert!(usage_should_retry_transport_error(
            &ureq::Error::ConnectionFailed
        ));
        assert!(!usage_should_retry_transport_error(
            &ureq::Error::StatusCode(503)
        ));
        assert!(usage_should_retry_status(429));
        assert!(usage_should_retry_status(503));
        assert!(!usage_should_retry_status(400));
    }

    #[test]
    fn usage_bucket_order_labels_and_colored_formatting_paths() {
        let other = UsageBucket {
            limit_id: "other".to_string(),
            label: "   ".to_string(),
            rate_limit: None,
        };
        let codex = UsageBucket {
            limit_id: "codex".to_string(),
            label: "codex".to_string(),
            rate_limit: None,
        };
        let ordered = ordered_usage_buckets(vec![other, codex]);
        assert_eq!(ordered[0].limit_id, "codex");
        assert_eq!(usage_bucket_label(&ordered[1]), "unknown");

        let _plain = set_plain_guard(false);
        assert!(format_usage_unavailable("unavailable", true).contains("unavailable"));
        assert!(style_usage_bar_with_color("bar", 80.0, true).contains("bar"));
        assert!(style_usage_bar_with_color("bar", 50.0, true).contains("bar"));
        assert!(style_usage_bar_with_color("bar", 10.0, true).contains("bar"));

        let colored = UsageLine {
            bar: "bar".to_string(),
            percent: "50%".to_string(),
            reset: "soon".to_string(),
            left_percent: Some(50),
        };
        assert!(!format_usage_line(&colored, false, true).is_empty());
        let empty_percent = UsageLine {
            bar: "bar".to_string(),
            percent: String::new(),
            reset: String::new(),
            left_percent: Some(50),
        };
        assert!(!format_usage_line(&empty_percent, true, true).is_empty());
        drop(_plain);

        let _plain = set_plain_guard(true);
        let plain = format_usage_line(&colored, false, false);
        assert!(plain.contains("left"));
    }

    #[test]
    fn usage_labels_follow_actual_window_duration() {
        for (seconds, label) in [
            (604_800, "Weekly"),
            (2_592_000, "Monthly"),
            (172_800, "2 day"),
            (18_000, "5 hour"),
            (900, "15 minute"),
            (90, "90 second"),
            (0, "Primary"),
            (-1, "Primary"),
        ] {
            assert_eq!(usage_window_label(seconds, false), label);
        }
        assert_eq!(usage_window_label(0, true), "Secondary");
    }

    #[test]
    fn weekly_only_window_preserves_position_and_duration() {
        let _plain = set_plain_guard(true);
        let payload: UsagePayload = serde_json::from_value(serde_json::json!({
            "rate_limit": {
                "secondary_window": {
                    "used_percent": 25,
                    "limit_window_seconds": 604800,
                    "reset_at": 2000000000
                }
            }
        }))
        .unwrap();
        let buckets = usage_snapshot_from_payload(&payload);
        assert!(buckets[0].primary.is_none());
        assert!(buckets[0].five_hour.is_none());
        assert_eq!(
            buckets[0].secondary.as_ref().unwrap().window_seconds,
            604800
        );
        assert_eq!(buckets[0].weekly.as_ref().unwrap().left_percent, 75);
        let lines = usage_lines_from_payload(&payload, "unavailable", Local::now());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("Weekly: 75% left"));
    }

    #[test]
    fn usage_windows_keep_upstream_order_with_accurate_compatibility_aliases() {
        let payload: UsagePayload = serde_json::from_value(serde_json::json!({
            "rate_limit": {
                "primary_window": {"used_percent": 70, "limit_window_seconds": 604800, "reset_at": 1},
                "secondary_window": {"used_percent": 20, "limit_window_seconds": 18000, "reset_at": 2}
            },
            "additional_rate_limits": [{
                "metered_feature": "custom",
                "rate_limit": {
                    "primary_window": {"used_percent": 5, "limit_window_seconds": 900, "reset_at": 3}
                }
            }]
        })).unwrap();
        let buckets = usage_snapshot_from_payload(&payload);
        assert_eq!(buckets[0].primary.as_ref().unwrap().window_seconds, 604800);
        assert_eq!(buckets[0].secondary.as_ref().unwrap().window_seconds, 18000);
        assert_eq!(buckets[0].weekly.as_ref().unwrap().left_percent, 30);
        assert_eq!(buckets[0].five_hour.as_ref().unwrap().left_percent, 80);
        assert_eq!(buckets[1].primary.as_ref().unwrap().window_seconds, 900);
        assert!(buckets[1].five_hour.is_none());
        assert!(buckets[1].weekly.is_none());
    }

    #[test]
    fn usage_limits_and_formatting() {
        let payload = UsagePayload {
            rate_limit: None,
            additional_rate_limits: None,
        };
        let limits = build_usage_limits(&payload);
        assert!(limits.primary.is_none());

        let window = RateLimitWindowSnapshot {
            used_percent: 50.0,
            limit_window_seconds: 10,
            reset_at: Local::now().timestamp(),
        };
        let rate_limit = RateLimitDetails {
            primary_window: Some(window.clone()),
            secondary_window: Some(window.clone()),
        };
        let payload = UsagePayload {
            rate_limit: Some(rate_limit),
            additional_rate_limits: None,
        };
        let limits = build_usage_limits(&payload);
        assert!(limits.primary.is_some());
        let line = format_limit(limits.primary.as_ref(), Local::now(), "none");
        assert!(line.left_percent.is_some());

        let invalid_reset = UsageWindow {
            left_percent: 50.0,
            reset_at: i64::MAX,
            window_seconds: 900,
        };
        assert_eq!(
            format_limit(Some(&invalid_reset), Local::now(), "none").reset,
            "unknown"
        );
    }

    #[test]
    fn usage_limits_fallback_to_additional_bucket_when_primary_missing() {
        let window = RateLimitWindowSnapshot {
            used_percent: 25.0,
            limit_window_seconds: 900,
            reset_at: Local::now().timestamp(),
        };
        let payload = UsagePayload {
            rate_limit: None,
            additional_rate_limits: Some(vec![AdditionalRateLimitDetails {
                limit_name: Some("codex_other".to_string()),
                metered_feature: Some("codex_other".to_string()),
                rate_limit: Some(RateLimitDetails {
                    primary_window: Some(window),
                    secondary_window: None,
                }),
            }]),
        };
        let limits = build_usage_limits(&payload);
        assert!(limits.primary.is_some());
    }

    #[test]
    fn usage_lines_include_multi_bucket_labels() {
        let _plain = set_plain_guard(true);
        let now = Local::now();
        let payload = UsagePayload {
            rate_limit: Some(RateLimitDetails {
                primary_window: Some(RateLimitWindowSnapshot {
                    used_percent: 20.0,
                    limit_window_seconds: 18000,
                    reset_at: now.timestamp() + 600,
                }),
                secondary_window: None,
            }),
            additional_rate_limits: Some(vec![AdditionalRateLimitDetails {
                limit_name: Some("codex_other".to_string()),
                metered_feature: Some("codex_other".to_string()),
                rate_limit: Some(RateLimitDetails {
                    primary_window: Some(RateLimitWindowSnapshot {
                        used_percent: 60.0,
                        limit_window_seconds: 3600,
                        reset_at: now.timestamp() + 900,
                    }),
                    secondary_window: None,
                }),
            }]),
        };
        let lines = usage_lines_from_payload(&payload, "unavailable", now);
        assert!(lines.iter().any(|line| line == "codex"));
        assert!(lines.iter().any(|line| line == "codex_other"));
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("  ") && line.contains("left"))
        );
    }

    #[test]
    fn usage_lines_label_dual_windows_for_single_bucket() {
        let _plain = set_plain_guard(true);
        let now = Local::now();
        let payload = UsagePayload {
            rate_limit: Some(RateLimitDetails {
                primary_window: Some(RateLimitWindowSnapshot {
                    used_percent: 20.0,
                    limit_window_seconds: 18000,
                    reset_at: now.timestamp() + 600,
                }),
                secondary_window: Some(RateLimitWindowSnapshot {
                    used_percent: 50.0,
                    limit_window_seconds: 604800,
                    reset_at: now.timestamp() + 3600,
                }),
            }),
            additional_rate_limits: None,
        };
        let lines = usage_lines_from_payload(&payload, "unavailable", now);
        assert!(lines.iter().any(|line| line.starts_with("5 hour: ")));
        assert!(lines.iter().any(|line| line.starts_with("Weekly: ")));
    }

    #[test]
    fn usage_unavailable_paths() {
        let _plain = set_plain_guard(true);
        assert_eq!(usage_unavailable(), "Data not available");
        let text = format_usage_unavailable("text", false);
        assert!(text.contains("Info"));
    }

    #[test]
    fn format_usage_variants() {
        let unavailable = "unavailable";
        let lines = format_usage(
            UsageLine::unavailable(unavailable),
            UsageLine::unavailable(unavailable),
            unavailable,
        );
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn format_usage_line_plain_and_dim() {
        let line = UsageLine {
            bar: render_bar(50.0),
            percent: "50%".to_string(),
            reset: "soon".to_string(),
            left_percent: Some(50),
        };
        let _plain = set_plain_guard(true);
        let plain = format_usage_line(&line, false, false);
        assert!(plain.contains("left"));
    }

    #[test]
    fn style_bar_and_strip_ansi() {
        let _env = set_env_guard("NO_COLOR", Some("1"));
        let bar = render_bar(10.0);
        let styled = style_usage_bar(&bar, 10.0);
        assert_eq!(bar, styled);
        let stripped = crate::ui::strip_ansi("\x1b[31mred\x1b[0m");
        assert_eq!(stripped, "red");
    }

    #[test]
    fn format_reset_timestamp_helpers() {
        use chrono::Timelike;
        let now = Local::now()
            .with_hour(12)
            .and_then(|value| value.with_minute(0))
            .and_then(|value| value.with_second(0))
            .and_then(|value| value.with_nanosecond(0))
            .expect("valid midday");
        let same_day = format_reset_timestamp(now.timestamp() + 60, now).expect("same day");
        let cross_day =
            format_reset_timestamp(now.timestamp() + 60 * 60 * 24, now).expect("cross day");
        assert!(same_day.contains(':'));
        assert!(!same_day.contains(" on "));
        assert!(cross_day.contains(" on "));
        assert!(local_from_timestamp(0).is_some());
        assert!(local_from_timestamp(-1).is_some());
    }

    #[test]
    fn lock_usage_failure_paths() {
        let _guard = LOCK_TEST_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = make_paths(dir.path());
        fs::create_dir_all(&paths.profiles).unwrap();
        fs::write(&paths.profiles_lock, "").unwrap();

        LOCK_FAILPOINT.with(|failpoint| failpoint.set(LOCK_FAIL_BUSY));
        let err = lock_usage(&paths).unwrap_err();
        assert!(err.contains("Could not acquire profiles lock"));
        LOCK_FAILPOINT.with(|failpoint| failpoint.set(LOCK_FAIL_ERR));
        let err = lock_usage(&paths).unwrap_err();
        assert!(err.contains("Could not lock profiles file"));
        LOCK_FAILPOINT.with(|failpoint| failpoint.set(0));
    }

    #[test]
    fn lock_usage_open_error() {
        let _guard = LOCK_TEST_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path().join("locked");
        fs::write(&lock_dir, "not a directory").unwrap();
        let mut paths = make_paths(dir.path());
        paths.profiles_lock = lock_dir.join("profiles.lock");
        let err = lock_usage(&paths).unwrap_err();
        assert!(err.contains("Could not open profiles lock"));
    }
}
