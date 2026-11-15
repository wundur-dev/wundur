//! A Wundur account: signing in to the hosted Wundur Studio API, and calling it.
//!
//! Signing in is OAuth 2.0's authorization code flow with PKCE (S256) in the system browser:
//! [`Account::begin_sign_in`] opens a loopback listener on a free port (127.0.0.1 and ::1) and
//! gives the address to open; [`Account::finish_sign_in`] waits for the browser to come back with
//! the code, checks `state`, trades the code for tokens, keeps the refresh token in the system's
//! key store ([`crate::secrets`]) and makes this machine's API key (`kel_…`), kept there too.
//!
//! The access token lives in memory only, with its expiry; it is refreshed shortly before it
//! expires and when the API answers `401`. Entra hands out a new refresh token on each refresh,
//! which replaces the kept one. A refresh refused with `invalid_grant` signs the account out.
//!
//! Background work (summaries, images, models) uses this machine's key: [`Account::api_key`]
//! and [`Account::send`] with [`Auth::Key`]. Tokens and keys are never logged, nor put in
//! settings or in error messages.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::Digest as _;

/// The hosted API, until it moves to `https://api.wundur.ai`.
pub const DEFAULT_API_BASE: &str = "https://wundur-api.greenmoss-5be352a8.westus.azurecontainerapps.io";
const AUTHORIZE_URL: &str = "https://wundurapi.ciamlogin.com/ea0a2010-c331-4354-beb2-375e88ffff9f/oauth2/v2.0/authorize";
const TOKEN_URL: &str = "https://wundurapi.ciamlogin.com/ea0a2010-c331-4354-beb2-375e88ffff9f/oauth2/v2.0/token";
/// The desktop app's registration: a public client, no secret.
const CLIENT_ID: &str = "9e5707f1-b23d-4859-bde2-4e739ddd9a03";
const SCOPE: &str = "api://eef7bdbb-5076-448f-8775-86c7adef384f/access openid offline_access profile";

/// How long the browser has to come back before signing in gives up.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// An access token this close to its expiry is refreshed first.
const EXPIRY_MARGIN: Duration = Duration::from_secs(120);

/// The key store's names for the account's secrets.
const REFRESH_TOKEN: &str = "wundur-account-refresh-token";
const API_KEY: &str = "wundur-account-api-key";

/// The account's settings (in Wundur's settings file; nothing secret).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The API's address; empty for [`DEFAULT_API_BASE`].
    pub api_base: String,
    /// The id of this machine's API key (the key itself is in the key store), to revoke it on
    /// signing out.
    pub key_id: String,
}

impl Config {
    /// The API's address in use.
    pub fn api_base(&self) -> &str {
        let b = self.api_base.trim();
        if b.is_empty() { DEFAULT_API_BASE } else { b }
    }
}

/// Where signing in and the API are.
#[derive(Clone, Debug, PartialEq)]
pub struct Endpoints {
    pub api_base: String,
    pub authorize: String,
    pub token: String,
    pub client_id: String,
    pub scope: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            api_base: DEFAULT_API_BASE.into(),
            authorize: AUTHORIZE_URL.into(),
            token: TOKEN_URL.into(),
            client_id: CLIENT_ID.into(),
            scope: SCOPE.into(),
        }
    }
}

// ----- errors -----

/// An error from the API (`{"error": {"code", "message"}}`) or the sign-in service, with its
/// HTTP status, so callers can act on the code (`invalid_token`, `image_allowance_used`,
/// `hourly_limit`, …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    /// Seconds the server asked to wait (`Retry-After`, sent with `busy`).
    pub retry_after: Option<u32>,
}

impl ApiError {
    /// Read an error body; one not in the API's shape keeps its status (`http_<status>`).
    pub fn parse(status: u16, body: &str) -> Self {
        #[derive(Deserialize)]
        struct Wrapped {
            error: Inner,
        }
        #[derive(Deserialize)]
        struct Inner {
            #[serde(default)]
            code: String,
            #[serde(default)]
            message: String,
        }
        // The sign-in service's errors: `{"error": "invalid_grant", "error_description": …}`.
        #[derive(Deserialize)]
        struct OAuth {
            error: String,
            #[serde(default)]
            error_description: String,
        }
        if let Ok(w) = serde_json::from_str::<Wrapped>(body) {
            return Self { status, code: w.error.code, message: w.error.message, retry_after: None };
        }
        if let Ok(o) = serde_json::from_str::<OAuth>(body) {
            // Entra's descriptions run to trace ids and timestamps: the first line says it.
            let message = o.error_description.lines().next().unwrap_or("").to_string();
            return Self { status, code: o.error, message, retry_after: None };
        }
        Self { status, code: format!("http_{status}"), message: String::new(), retry_after: None }
    }

    /// A rate limit was reached (`429`: the hourly limit, the generation cap): try later.
    pub fn is_limit(&self) -> bool {
        self.status == 429
    }

    /// Something is used up for this month (`402`, or an allowance code an older API sent with
    /// `429`): more needs an upgrade.
    pub fn is_used_up(&self) -> bool {
        self.status == 402 || self.code.ends_with("_allowance_used") || self.code.ends_with("_credits_used") || self.code.ends_with("_seconds_used")
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.message.is_empty() {
            write!(f, "{} ({})", self.code, self.status)
        } else {
            write!(f, "{} ({}, {})", self.message, self.code, self.status)
        }
    }
}

/// What can go wrong. None of these carry a token or key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The API or the sign-in service answered with an error.
    Api(ApiError),
    /// Not signed in (or no longer: the refresh token was refused), or no API key yet.
    SignedOut,
    /// The browser came back with an error (`access_denied`: the person cancelled).
    Denied { error: String, description: String },
    /// Signing in was cancelled here.
    Cancelled,
    /// The browser didn't come back in time.
    TimedOut,
    /// The network, or an answer that couldn't be read.
    Network(String),
    /// The system's key store refused.
    Store(String),
}

impl Error {
    /// The API's error code, if it answered with one.
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Api(e) => Some(&e.code),
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Api(e) => e.fmt(f),
            Error::SignedOut => f.write_str("not signed in"),
            Error::Denied { error, description } if error == "access_denied" && description.is_empty() => f.write_str("sign-in was cancelled in the browser"),
            Error::Denied { error, description } if description.is_empty() => write!(f, "the sign-in page said {error}"),
            Error::Denied { error, description } => write!(f, "the sign-in page said {error}: {description}"),
            Error::Cancelled => f.write_str("signing in was cancelled"),
            Error::TimedOut => f.write_str("the browser didn't come back in time"),
            Error::Network(e) => write!(f, "can't reach the server: {e}"),
            Error::Store(e) => write!(f, "the key store: {e}"),
        }
    }
}

impl std::error::Error for Error {}

// ----- what the API answers -----

/// A count with a limit (`{used, limit, left}`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Usage {
    #[serde(deserialize_with = "number")]
    pub used: i64,
    #[serde(deserialize_with = "number")]
    pub limit: i64,
    #[serde(deserialize_with = "number")]
    pub left: i64,
}

/// What's used and left of the month's allowance.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Allowance {
    pub images: Usage,
    /// Spent by Import, by each engine's credits (`None` from an API before credit buckets).
    pub model_credits: Option<Usage>,
    /// Sound effects, in seconds.
    pub sound_seconds: Option<Usage>,
    pub summaries: Usage,
    pub summary_tokens: Usage,
    /// From an API before credit buckets: models imported, and generations.
    pub approved_models: Usage,
    pub attempts: Usage,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Limits {
    #[serde(deserialize_with = "number")]
    pub models_at_once: i64,
    #[serde(deserialize_with = "number")]
    pub images_per_hour: i64,
    #[serde(deserialize_with = "number")]
    pub summaries_per_hour: i64,
    pub summary_thinking: bool,
}

/// `GET /v1/me`: the account, its plan and what's left this month. Read leniently: fields may
/// be added, and missing or null ones are their defaults.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Me {
    #[serde(deserialize_with = "text")]
    pub account_id: String,
    #[serde(deserialize_with = "text")]
    pub email: String,
    /// `free`, `comp` or `monthly` (or a plan added later).
    #[serde(deserialize_with = "text")]
    pub plan: String,
    pub paid: bool,
    /// The subscription's status (`active`, `past_due`, `canceled`, …); empty when none.
    #[serde(deserialize_with = "text")]
    pub status: String,
    /// When the paid period ends (ISO 8601), for paid plans.
    pub period_end: Option<String>,
    pub allowance: Allowance,
    /// What a model attempt costs per engine (`trellis2`, `tripo`).
    pub attempt_weights: BTreeMap<String, f64>,
    pub limits: Limits,
    pub can_manage_billing: bool,
}

impl Me {
    /// The plan's name to show: Free, Comp, Monthly.
    pub fn plan_name(&self) -> String {
        match self.plan.as_str() {
            "" | "free" => "Free".into(),
            "comp" => "Comp".into(),
            "monthly" => "Monthly".into(),
            other => {
                let mut c = other.chars();
                c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
            }
        }
    }

    /// On the free plan (so offered the upgrade).
    pub fn is_free(&self) -> bool {
        matches!(self.plan.as_str(), "" | "free")
    }

    /// The status, when it is worth saying (not active), in words: "past due".
    pub fn status_note(&self) -> Option<String> {
        let s = self.status.trim();
        (!s.is_empty() && s != "active" && s != "free" && s != "trialing").then(|| s.replace('_', " "))
    }
}

/// An API key as listed (never the key itself).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct KeyInfo {
    #[serde(deserialize_with = "text")]
    pub id: String,
    #[serde(deserialize_with = "text")]
    pub name: String,
    #[serde(deserialize_with = "text")]
    pub last4: String,
    #[serde(deserialize_with = "text")]
    pub created_at: String,
}

/// A key just made: the key itself is here only.
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct NewKey {
    #[serde(flatten)]
    pub info: KeyInfo,
    pub key: String,
}

impl std::fmt::Debug for NewKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewKey").field("info", &self.info).field("key", &"<hidden>").finish()
    }
}

/// A number, or null (0); floats are rounded down.
fn number<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(0),
        _ => 0,
    })
}

/// A string, or null (empty).
fn text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => s,
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    })
}

// ----- PKCE, the authorize address, the redirect -----

/// `n` random bytes, base64url without padding.
fn random_text(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).expect("the system's random numbers");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE's S256 challenge for `verifier` (RFC 7636): base64url(sha256(verifier)), no padding.
pub fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// Percent-encode everything but RFC 3986's unreserved characters.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Undo percent-encoding (and `+` for a space, as forms send it).
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The address to open in the browser to sign in.
pub fn authorize_url(endpoints: &Endpoints, redirect_uri: &str, challenge: &str, state: &str) -> String {
    let params = [
        ("client_id", endpoints.client_id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("response_mode", "query"),
        ("scope", endpoints.scope.as_str()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect();
    format!("{}?{}", endpoints.authorize, query.join("&"))
}

/// What came back to the loopback listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Callback {
    /// The code to trade for tokens.
    Code(String),
    /// The sign-in page sent an error back (with the right state).
    Denied { error: String, description: String },
    /// A request with the wrong (or no) state: refused, and signing in keeps waiting.
    StateMismatch,
    /// Something else (a favicon, a stray request).
    Other,
}

/// Read a request line (`GET /?code=…&state=… HTTP/1.1`) that came to the listener.
pub fn parse_callback(request_line: &str, state: &str) -> Callback {
    let mut parts = request_line.split_whitespace();
    let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else { return Callback::Other };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/" {
        return Callback::Other;
    }
    let params: HashMap<String, String> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect();
    let (code, error) = (params.get("code"), params.get("error"));
    if code.is_none() && error.is_none() {
        return Callback::Other;
    }
    // Compared in constant time: the state is what stops a forged redirect.
    let given = params.get("state").map(String::as_str).unwrap_or("");
    let same = given.len() == state.len() && given.bytes().zip(state.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
    if !same || state.is_empty() {
        return Callback::StateMismatch;
    }
    match (code, error) {
        (_, Some(error)) => Callback::Denied { error: error.clone(), description: params.get("error_description").cloned().unwrap_or_default() },
        (Some(code), None) if !code.is_empty() => Callback::Code(code.clone()),
        _ => Callback::Other,
    }
}

// ----- the access token -----

/// The access token, in memory only.
#[derive(Clone)]
pub struct AccessToken {
    value: String,
    expires: Instant,
}

impl AccessToken {
    pub fn new(value: String, expires_in: Duration, now: Instant) -> Self {
        Self { value, expires: now + expires_in }
    }

    /// Good to use at `now`: not within [`EXPIRY_MARGIN`] of its expiry.
    pub fn fresh(&self, now: Instant) -> bool {
        now + EXPIRY_MARGIN < self.expires
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessToken").field("value", &"<hidden>").field("expires", &self.expires).finish()
    }
}

/// The token endpoint's answer.
#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    /// Seconds; Entra sends a number (some servers a string).
    #[serde(default, deserialize_with = "number")]
    expires_in: i64,
}

impl Tokens {
    fn access(&self, now: Instant) -> AccessToken {
        // No expiry given: assume Entra's usual hour, less to be safe.
        let secs = if self.expires_in > 0 { self.expires_in as u64 } else { 3000 };
        AccessToken::new(self.access_token.clone(), Duration::from_secs(secs), now)
    }
}

// ----- where the secrets are kept -----

/// Where the refresh token and the key are kept: the system's key store, or memory (tests).
pub trait Store: Send + Sync {
    fn get(&self, name: &str) -> Option<String>;
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
    fn remove(&self, name: &str);
}

/// The system's key store ([`crate::secrets`]).
pub struct Keychain;

impl Store for Keychain {
    fn get(&self, name: &str) -> Option<String> {
        crate::secrets::get(name)
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        crate::secrets::set(name, value).map_err(|e| e.to_string())
    }
    fn remove(&self, name: &str) {
        crate::secrets::remove(name)
    }
}

/// Secrets in memory, for tests.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl Store for MemoryStore {
    fn get(&self, name: &str) -> Option<String> {
        self.0.lock().ok()?.get(name).cloned()
    }
    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        self.0.lock().map_err(|_| "poisoned".to_string())?.insert(name.into(), value.into());
        Ok(())
    }
    fn remove(&self, name: &str) {
        if let Ok(mut m) = self.0.lock() {
            m.remove(name);
        }
    }
}

// ----- signing in -----

/// Stops a sign-in that is waiting for the browser.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    fn cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A sign-in under way: the loopback listener, waiting for the browser.
pub struct SignIn {
    /// The address to open in the browser.
    pub url: String,
    redirect_uri: String,
    verifier: String,
    state: String,
    listeners: Vec<TcpListener>,
    cancel: Cancel,
}

impl SignIn {
    /// The redirect address (`http://localhost:<port>/`).
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// The listener's port.
    pub fn port(&self) -> u16 {
        self.listeners.first().and_then(|l| l.local_addr().ok()).map_or(0, |a| a.port())
    }

    /// What stops it waiting.
    pub fn canceller(&self) -> Cancel {
        self.cancel.clone()
    }

    /// Wait for the browser to come back with a code, answering it; until `timeout` or
    /// cancelled.
    pub fn wait_for_code(&self, timeout: Duration) -> Result<String, Error> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.cancel.cancelled() {
                return Err(Error::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(Error::TimedOut);
            }
            let mut idle = true;
            for l in &self.listeners {
                match l.accept() {
                    Ok((stream, _)) => {
                        idle = false;
                        if let Some(done) = self.answer(stream) {
                            return done;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => {}
                }
            }
            if idle {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    /// Answer one request; `Some` when signing in is decided by it.
    fn answer(&self, mut stream: TcpStream) -> Option<Result<String, Error>> {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        let mut head = Vec::new();
        let mut buf = [0u8; 2048];
        while head.len() < 16 * 1024 && !head.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        let head = String::from_utf8_lossy(&head);
        let line = head.lines().next().unwrap_or("");
        let (status, title, text, done) = match parse_callback(line, &self.state) {
            Callback::Code(code) => ("200 OK", "Signed in", "Signed in to Wundur Studio. You can close this tab.", Some(Ok(code))),
            Callback::Denied { error, description } => {
                ("200 OK", "Not signed in", "Signing in to Wundur Studio didn't finish. You can close this tab and try again from the app.", Some(Err(Error::Denied { error, description })))
            }
            Callback::StateMismatch => ("400 Bad Request", "Not signed in", "This sign-in doesn't match the one Wundur Studio started, so it was refused.", None),
            Callback::Other => ("404 Not Found", "Not found", "Nothing here.", None),
        };
        let body = format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
             <body style=\"font-family:-apple-system,system-ui,sans-serif;display:flex;align-items:center;justify-content:center;height:90vh;color:#222\">\
             <div style=\"text-align:center\"><h2>{title}</h2><p>{text}</p></div></body></html>"
        );
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(reply.as_bytes());
        let _ = stream.flush();
        done
    }
}

/// Listen on the same free port on 127.0.0.1 and ::1 (browsers resolve `localhost` to either);
/// IPv4 alone when this machine has no IPv6 loopback.
fn loopback_listeners() -> io::Result<Vec<TcpListener>> {
    let mut last = None;
    for _ in 0..20 {
        let v4 = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
        let port = v4.local_addr()?.port();
        match TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))) {
            Ok(v6) => return finish_listeners(vec![v4, v6]),
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => last = Some(e),
            // No IPv6 loopback here.
            Err(_) => return finish_listeners(vec![v4]),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no free port")))
}

fn finish_listeners(listeners: Vec<TcpListener>) -> io::Result<Vec<TcpListener>> {
    for l in &listeners {
        l.set_nonblocking(true)?;
    }
    Ok(listeners)
}

/// This machine's name, for its key's name ("Wundur Studio on Stewart's MacBook").
pub fn machine_name() -> String {
    let run = |program: &str, args: &[&str]| {
        crate::platform::command(program)
            .args(args)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let name = if cfg!(target_os = "macos") { run("scutil", &["--get", "ComputerName"]) } else { None };
    name.or_else(|| std::env::var("COMPUTERNAME").ok().filter(|s| !s.is_empty()))
        .or_else(|| run("hostname", &[]))
        .unwrap_or_else(|| "this computer".into())
}

/// The name this machine's key gets (the API takes up to 100 characters).
fn key_name() -> String {
    format!("Wundur Studio on {}", machine_name()).chars().take(100).collect()
}

// ----- the account -----

/// How a call is authorised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    /// The signed-in session's access token (refreshed as needed).
    Session,
    /// This machine's API key (`kel_…`), for background work.
    Key,
}

/// After signing in: the account, and this machine's key (or why it couldn't be made).
#[derive(Debug)]
pub struct SignedIn {
    /// `None` when `/v1/me` failed (signed in all the same).
    pub me: Option<Me>,
    /// This machine's key, made and kept: its info (the id goes in [`Config::key_id`]).
    pub key: Result<KeyInfo, Error>,
}

/// A Wundur account on this machine: its tokens and key, and the API.
pub struct Account {
    endpoints: RwLock<Endpoints>,
    store: Box<dyn Store>,
    access: Mutex<Option<AccessToken>>,
    /// Held while refreshing, so one refresh runs at a time (each replaces the refresh token).
    refreshing: Mutex<()>,
    /// Whether a refresh token is kept, and the key, as last read (`None`: not read yet).
    signed_in: Mutex<Option<bool>>,
    key: Mutex<Option<Option<String>>>,
}

static SHARED: OnceLock<Account> = OnceLock::new();

/// The app's account: the default endpoints and the system's key store.
pub fn shared() -> &'static Account {
    SHARED.get_or_init(|| Account::new(Endpoints::default(), Box::new(Keychain)))
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build().into()
}

/// The body of an answer, or an [`ApiError`] when its status says it failed.
fn read(resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<String, Error> {
    let mut resp = resp.map_err(|e| Error::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u32>().ok());
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    if (200..300).contains(&status) { Ok(body) } else { Err(Error::Api(ApiError { retry_after, ..ApiError::parse(status, &body) })) }
}

fn parse<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, Error> {
    serde_json::from_str(body).map_err(|e| Error::Network(format!("an answer that couldn't be read ({e})")))
}

impl Account {
    pub fn new(endpoints: Endpoints, store: Box<dyn Store>) -> Self {
        Self { endpoints: RwLock::new(endpoints), store, access: Mutex::new(None), refreshing: Mutex::new(()), signed_in: Mutex::new(None), key: Mutex::new(None) }
    }

    fn endpoints(&self) -> Endpoints {
        self.endpoints.read().map(|e| e.clone()).unwrap_or_default()
    }

    /// Use the API at `base` (empty: the default). From [`Config::api_base`].
    pub fn set_api_base(&self, base: &str) {
        let base = base.trim().trim_end_matches('/');
        if let Ok(mut e) = self.endpoints.write() {
            e.api_base = if base.is_empty() { DEFAULT_API_BASE.into() } else { base.into() };
        }
    }

    /// The API's address in use.
    pub fn api_base(&self) -> String {
        self.endpoints().api_base
    }

    /// Signed in on this machine (a refresh token is kept). Asks the key store the first time
    /// (a process: call it off the UI thread), then remembers.
    pub fn is_signed_in(&self) -> bool {
        if let Some(known) = self.signed_in.lock().ok().and_then(|s| *s) {
            return known;
        }
        let kept = self.store.get(REFRESH_TOKEN).is_some();
        if let Ok(mut s) = self.signed_in.lock() {
            *s = Some(kept);
        }
        kept
    }

    /// Whether signed in, as known without asking the key store (`None`: not asked yet).
    pub fn known_signed_in(&self) -> Option<bool> {
        self.signed_in.lock().ok().and_then(|s| *s)
    }

    /// This machine's API key (`kel_…`), for background calls; `None` when signed out or not
    /// made yet. Asks the key store the first time, then remembers.
    pub fn api_key(&self) -> Option<String> {
        if let Some(known) = self.key.lock().ok().and_then(|k| k.clone()) {
            return known;
        }
        let key = self.store.get(API_KEY);
        if let Ok(mut k) = self.key.lock() {
            *k = Some(key.clone());
        }
        key
    }

    /// Start signing in: the loopback listener, and the address for the browser
    /// ([`SignIn::url`]; the caller opens it). Then [`Account::finish_sign_in`].
    pub fn begin_sign_in(&self) -> io::Result<SignIn> {
        let listeners = loopback_listeners()?;
        let port = listeners[0].local_addr()?.port();
        let redirect_uri = format!("http://localhost:{port}/");
        // 32 random bytes: a 43-character verifier, as RFC 7636 suggests.
        let verifier = random_text(32);
        let state = random_text(16);
        let url = authorize_url(&self.endpoints(), &redirect_uri, &pkce_challenge(&verifier), &state);
        Ok(SignIn { url, redirect_uri, verifier, state, listeners, cancel: Cancel::default() })
    }

    /// Wait for the browser (up to `timeout`), trade the code for tokens, keep the refresh
    /// token, and make and keep this machine's API key. Blocks: run it off the UI thread.
    pub fn finish_sign_in(&self, sign_in: SignIn, timeout: Duration) -> Result<SignedIn, Error> {
        let code = sign_in.wait_for_code(timeout)?;
        drop(sign_in.listeners);
        let ep = self.endpoints();
        let resp = agent(Duration::from_secs(30)).post(&ep.token).send_form([
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", sign_in.redirect_uri.as_str()),
            ("client_id", ep.client_id.as_str()),
            ("code_verifier", sign_in.verifier.as_str()),
            ("scope", ep.scope.as_str()),
        ]);
        let tokens: Tokens = parse(&read(resp)?)?;
        let refresh = tokens.refresh_token.clone().filter(|t| !t.is_empty()).ok_or_else(|| Error::Network("the sign-in service sent no refresh token".into()))?;
        self.store.set(REFRESH_TOKEN, &refresh).map_err(Error::Store)?;
        self.set_signed_in(true);
        self.set_access(Some(tokens.access(Instant::now())));
        let key = self.create_machine_key();
        let me = self.me().ok();
        Ok(SignedIn { me, key })
    }

    /// Make this machine's API key and keep it (replacing one kept). Needs the session.
    pub fn create_machine_key(&self) -> Result<KeyInfo, Error> {
        let new = self.create_key(&key_name())?;
        if new.key.is_empty() {
            return Err(Error::Network("the server sent no key".into()));
        }
        self.store.set(API_KEY, &new.key).map_err(Error::Store)?;
        if let Ok(mut k) = self.key.lock() {
            *k = Some(Some(new.key.clone()));
        }
        Ok(new.info)
    }

    /// Sign out: revoke this machine's key `key_id` (best effort), then forget the tokens and
    /// the key. Blocks (the revoke is a network call).
    pub fn sign_out(&self, key_id: Option<&str>) {
        if let Some(id) = key_id.filter(|id| !id.is_empty()) {
            // With the key itself when there is one: it works even once the session is gone.
            let auth = if self.api_key().is_some() { Auth::Key } else { Auth::Session };
            let _ = self.send(auth, "DELETE", &format!("/v1/keys/{}", encode(id)), None);
        }
        self.forget_session();
        self.store.remove(API_KEY);
        if let Ok(mut k) = self.key.lock() {
            *k = Some(None);
        }
    }

    /// Forget the refresh and access tokens (the key stays until [`Account::sign_out`]).
    fn forget_session(&self) {
        self.store.remove(REFRESH_TOKEN);
        self.set_signed_in(false);
        self.set_access(None);
    }

    fn set_signed_in(&self, on: bool) {
        if let Ok(mut s) = self.signed_in.lock() {
            *s = Some(on);
        }
    }

    fn set_access(&self, token: Option<AccessToken>) {
        if let Ok(mut a) = self.access.lock() {
            *a = token;
        }
    }

    /// A good access token: the one held, else a refreshed one.
    fn access_token(&self, force: bool) -> Result<String, Error> {
        if !force && let Some(t) = self.access.lock().ok().and_then(|a| a.clone()).filter(|t| t.fresh(Instant::now())) {
            return Ok(t.value);
        }
        self.refresh(force)
    }

    /// Trade the refresh token for a new access token (and refresh token, kept). `invalid_grant`
    /// signs out: [`Error::SignedOut`].
    fn refresh(&self, force: bool) -> Result<String, Error> {
        let _one = self.refreshing.lock().map_err(|_| Error::SignedOut)?;
        // Another call may have refreshed while this one waited.
        if !force && let Some(t) = self.access.lock().ok().and_then(|a| a.clone()).filter(|t| t.fresh(Instant::now())) {
            return Ok(t.value);
        }
        let Some(refresh) = self.store.get(REFRESH_TOKEN) else {
            self.set_signed_in(false);
            return Err(Error::SignedOut);
        };
        let ep = self.endpoints();
        let resp = agent(Duration::from_secs(30)).post(&ep.token).send_form([
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", ep.client_id.as_str()),
            ("scope", ep.scope.as_str()),
        ]);
        let tokens: Tokens = match read(resp) {
            Ok(body) => parse(&body)?,
            Err(Error::Api(e)) if e.code == "invalid_grant" => {
                self.forget_session();
                return Err(Error::SignedOut);
            }
            Err(e) => return Err(e),
        };
        if let Some(new) = tokens.refresh_token.as_deref().filter(|t| !t.is_empty() && *t != refresh) {
            self.store.set(REFRESH_TOKEN, new).map_err(Error::Store)?;
        }
        let access = tokens.access(Instant::now());
        let value = access.value.clone();
        self.set_access(Some(access));
        Ok(value)
    }

    /// Call the API: `method` (`GET`, `POST`, `DELETE`) on `path` (`/v1/me`), with a JSON body
    /// or none. With [`Auth::Session`], a `401` refreshes the access token and tries once more.
    /// The answer's body (empty for `204`).
    pub fn send(&self, auth: Auth, method: &str, path: &str, body: Option<&serde_json::Value>) -> Result<String, Error> {
        self.send_within(auth, method, path, body, Duration::from_secs(60))
    }

    /// [`Self::send`], waiting up to `timeout` for the answer (a thinking summary takes minutes).
    pub fn send_within(&self, auth: Auth, method: &str, path: &str, body: Option<&serde_json::Value>, timeout: Duration) -> Result<String, Error> {
        self.request(auth, method, path, body, timeout, None)
    }

    /// A call to the API: `method` (GET, POST, PATCH, DELETE) on `path`, with `body` as JSON,
    /// waiting up to `timeout`; for a call that spends, an `idempotency_key` (new per action:
    /// the server answers a repeat of it with the first answer, so a retry can't charge twice).
    pub fn request(
        &self,
        auth: Auth,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        timeout: Duration,
        idempotency_key: Option<&str>,
    ) -> Result<String, Error> {
        let attempt = |force: bool| -> Result<String, Error> {
            let credential = match auth {
                Auth::Session => self.access_token(force)?,
                Auth::Key => self.api_key().ok_or(Error::SignedOut)?,
            };
            let url = format!("{}{}", self.api_base(), path);
            let bearer = format!("Bearer {credential}");
            let http = agent(timeout);
            let key = idempotency_key.unwrap_or_default();
            let resp = match (method, body) {
                ("GET", _) => http.get(&url).header("Authorization", &bearer).call(),
                ("DELETE", _) => http.delete(&url).header("Authorization", &bearer).call(),
                ("PATCH", b) => {
                    let req = http.patch(&url).header("Authorization", &bearer);
                    match b {
                        Some(b) => req.send_json(b),
                        None => req.send_empty(),
                    }
                }
                (_, Some(b)) if !key.is_empty() => http.post(&url).header("Authorization", &bearer).header("Idempotency-Key", key).send_json(b),
                (_, Some(b)) => http.post(&url).header("Authorization", &bearer).send_json(b),
                (_, None) if !key.is_empty() => http.post(&url).header("Authorization", &bearer).header("Idempotency-Key", key).send_empty(),
                (_, None) => http.post(&url).header("Authorization", &bearer).send_empty(),
            };
            read(resp)
        };
        match attempt(false) {
            Err(Error::Api(e)) if e.status == 401 && auth == Auth::Session => attempt(true),
            other => other,
        }
    }

    fn json<T: serde::de::DeserializeOwned>(&self, auth: Auth, method: &str, path: &str, body: Option<&serde_json::Value>) -> Result<T, Error> {
        parse(&self.send(auth, method, path, body)?)
    }

    /// `GET /v1/me` with the session.
    pub fn me(&self) -> Result<Me, Error> {
        self.json(Auth::Session, "GET", "/v1/me", None)
    }

    /// `GET /v1/me` with `auth` (background work checks what's left with [`Auth::Key`]).
    pub fn me_with(&self, auth: Auth) -> Result<Me, Error> {
        self.json(auth, "GET", "/v1/me", None)
    }

    /// `POST /v1/keys`: a new key named `name` (needs the session; the key is shown once).
    pub fn create_key(&self, name: &str) -> Result<NewKey, Error> {
        self.json(Auth::Session, "POST", "/v1/keys", Some(&serde_json::json!({ "name": name })))
    }

    /// `GET /v1/keys`: the account's keys (never the keys themselves).
    pub fn keys(&self) -> Result<Vec<KeyInfo>, Error> {
        #[derive(Deserialize)]
        struct Keys {
            #[serde(default)]
            keys: Vec<KeyInfo>,
        }
        Ok(self.json::<Keys>(Auth::Session, "GET", "/v1/keys", None)?.keys)
    }

    /// `DELETE /v1/keys/{id}`: the key stops working at once.
    pub fn delete_key(&self, id: &str) -> Result<(), Error> {
        self.send(Auth::Session, "DELETE", &format!("/v1/keys/{}", encode(id)), None).map(|_| ())
    }

    /// `POST /v1/checkout` for `plan` (`monthly`): the address of Stripe's checkout page.
    pub fn checkout(&self, plan: &str) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct Checkout {
            checkout_url: String,
        }
        Ok(self.json::<Checkout>(Auth::Session, "POST", "/v1/checkout", Some(&serde_json::json!({ "plan": plan })))?.checkout_url)
    }

    /// `POST /v1/billing/portal`: the address of Stripe's customer portal.
    pub fn billing_portal(&self) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct Portal {
            portal_url: String,
        }
        Ok(self.json::<Portal>(Auth::Session, "POST", "/v1/billing/portal", None)?.portal_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn me_reads_the_credit_buckets_and_the_older_counts() {
        let new: Me = serde_json::from_str(
            r#"{"plan":"free","allowance":{"images":{"used":1,"limit":5,"left":4},"model_credits":{"used":0,"limit":3,"left":3},"sound_seconds":{"used":10,"limit":60,"left":50}}}"#,
        )
        .unwrap();
        assert_eq!(new.allowance.model_credits.map(|u| u.left), Some(3));
        assert_eq!(new.allowance.sound_seconds.map(|u| u.left), Some(50));
        let old: Me = serde_json::from_str(r#"{"plan":"free","allowance":{"approved_models":{"used":0,"limit":1,"left":1}}}"#).unwrap();
        assert!(old.allowance.model_credits.is_none());
        assert_eq!(old.allowance.approved_models.left, 1);
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636() {
        // RFC 7636, appendix B.
        assert_eq!(pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn verifier_and_state_are_random_and_url_safe() {
        let (a, b) = (random_text(32), random_text(32));
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(a.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
    }

    #[test]
    fn the_authorize_url_has_what_entra_needs() {
        let url = authorize_url(&Endpoints::default(), "http://localhost:53122/", "CHALLENGE", "STATE");
        let (base, query) = url.split_once('?').unwrap();
        assert_eq!(base, AUTHORIZE_URL);
        let params: HashMap<String, String> = query.split('&').map(|p| p.split_once('=').unwrap()).map(|(k, v)| (k.to_string(), decode(v))).collect();
        assert_eq!(params["client_id"], CLIENT_ID);
        assert_eq!(params["response_type"], "code");
        assert_eq!(params["redirect_uri"], "http://localhost:53122/");
        assert_eq!(params["scope"], SCOPE);
        assert_eq!(params["code_challenge"], "CHALLENGE");
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["state"], "STATE");
        assert!(query.contains("redirect_uri=http%3A%2F%2Flocalhost%3A53122%2F"));
        assert!(query.contains("scope=api%3A%2F%2Feef7bdbb-5076-448f-8775-86c7adef384f%2Faccess%20openid%20offline_access%20profile"));
    }

    #[test]
    fn the_redirect_gives_the_code_when_the_state_matches() {
        assert_eq!(parse_callback("GET /?code=abc%2Fdef&state=s1 HTTP/1.1", "s1"), Callback::Code("abc/def".into()));
        assert_eq!(parse_callback("GET /?state=s1&code=xyz&session_state=q HTTP/1.1", "s1"), Callback::Code("xyz".into()));
    }

    #[test]
    fn a_redirect_with_another_state_is_refused() {
        assert_eq!(parse_callback("GET /?code=abc&state=other HTTP/1.1", "s1"), Callback::StateMismatch);
        assert_eq!(parse_callback("GET /?code=abc HTTP/1.1", "s1"), Callback::StateMismatch);
        assert_eq!(parse_callback("GET /?error=access_denied&state=other HTTP/1.1", "s1"), Callback::StateMismatch);
    }

    #[test]
    fn a_denied_sign_in_says_so() {
        assert_eq!(
            parse_callback("GET /?error=access_denied&error_description=The+user+cancelled&state=s1 HTTP/1.1", "s1"),
            Callback::Denied { error: "access_denied".into(), description: "The user cancelled".into() }
        );
    }

    #[test]
    fn other_requests_are_ignored() {
        assert_eq!(parse_callback("GET /favicon.ico HTTP/1.1", "s1"), Callback::Other);
        assert_eq!(parse_callback("GET / HTTP/1.1", "s1"), Callback::Other);
        assert_eq!(parse_callback("POST /?code=a&state=s1 HTTP/1.1", "s1"), Callback::Other);
        assert_eq!(parse_callback("", "s1"), Callback::Other);
    }

    #[test]
    fn decoding_handles_odd_input() {
        assert_eq!(decode("a%20b+c"), "a b c");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
        assert_eq!(decode("%e2%9c%93"), "✓");
    }

    #[test]
    fn me_is_read_leniently() {
        let body = r#"{"account_id":"a1","email":"p@example.com","plan":"monthly","paid":true,"status":"past_due",
            "period_end":"2026-12-01T00:00:00+00:00",
            "allowance":{"approved_models":{"used":2,"limit":25,"left":23},"attempts":{"used":3,"limit":50,"left":47},
              "images":{"used":0,"limit":300,"left":300},"summaries":{"used":10,"limit":5000,"left":4990},
              "summary_tokens":{"used":1000,"limit":20000000,"left":19999000}},
            "attempt_weights":{"trellis2":1,"tripo":2},
            "limits":{"models_at_once":1,"images_per_hour":60,"summaries_per_hour":120,"summary_thinking":true},
            "can_manage_billing":true,"something_new":{"x":1}}"#;
        let me: Me = serde_json::from_str(body).unwrap();
        assert_eq!(me.email, "p@example.com");
        assert_eq!(me.plan_name(), "Monthly");
        assert!(!me.is_free() && me.paid && me.can_manage_billing);
        assert_eq!(me.status_note().as_deref(), Some("past due"));
        assert_eq!(me.allowance.approved_models, Usage { used: 2, limit: 25, left: 23 });
        assert_eq!(me.allowance.images.left, 300);
        assert_eq!(me.attempt_weights["tripo"], 2.0);
        assert!(me.limits.summary_thinking);
        assert_eq!(me.period_end.as_deref(), Some("2026-12-01T00:00:00+00:00"));

        // A new account: nulls, missing fields.
        let me: Me = serde_json::from_str(r#"{"account_id":"a2","email":null,"plan":"free","status":null,"period_end":null}"#).unwrap();
        assert_eq!((me.email.as_str(), me.plan_name().as_str(), me.status_note()), ("", "Free", None));
        assert!(me.is_free() && !me.can_manage_billing);
        assert_eq!(me.allowance.images, Usage::default());
    }

    #[test]
    fn errors_are_read_into_their_code() {
        let e = ApiError::parse(401, r#"{"error":{"code":"missing_key","message":"Send Authorization: Bearer <key>."}}"#);
        assert_eq!((e.status, e.code.as_str(), e.message.as_str()), (401, "missing_key", "Send Authorization: Bearer <key>."));
        let e = ApiError::parse(429, r#"{"error":{"code":"image_allowance_used","message":"No images left."}}"#);
        assert!(e.is_limit());
        assert_eq!(Error::Api(e).code(), Some("image_allowance_used"));
        let e = ApiError::parse(400, r#"{"error":"invalid_grant","error_description":"AADSTS70008: expired.\r\nTrace ID: 1"}"#);
        assert_eq!((e.code.as_str(), e.message.as_str()), ("invalid_grant", "AADSTS70008: expired."));
        let e = ApiError::parse(502, "<html>Bad gateway</html>");
        assert_eq!((e.code.as_str(), e.message.as_str()), ("http_502", ""));
    }

    #[test]
    fn an_access_token_is_refreshed_before_it_expires() {
        let now = Instant::now();
        let t = AccessToken::new("t".into(), Duration::from_secs(3600), now);
        assert!(t.fresh(now));
        assert!(t.fresh(now + Duration::from_secs(3600) - EXPIRY_MARGIN - Duration::from_secs(1)));
        assert!(!t.fresh(now + Duration::from_secs(3600) - EXPIRY_MARGIN));
        assert!(!t.fresh(now + Duration::from_secs(4000)));
        assert!(!format!("{t:?}").contains("\"t\""), "never printed");
    }

    #[test]
    fn signed_in_state_and_key_come_from_the_store_and_go_on_signing_out() {
        let store = MemoryStore::default();
        store.set(REFRESH_TOKEN, "rt").unwrap();
        store.set(API_KEY, "kel_test").unwrap();
        let account = Account::new(Endpoints::default(), Box::new(store));
        assert!(account.is_signed_in());
        assert_eq!(account.api_key().as_deref(), Some("kel_test"));
        // No key id: nothing to revoke, so no network.
        account.sign_out(None);
        assert!(!account.is_signed_in());
        assert_eq!(account.api_key(), None);
        assert_eq!(account.store.get(REFRESH_TOKEN), None);
        assert!(matches!(account.me(), Err(Error::SignedOut)), "no session: no call");
    }

    #[test]
    fn the_api_address_is_a_setting() {
        let account = Account::new(Endpoints::default(), Box::new(MemoryStore::default()));
        assert_eq!(account.api_base(), DEFAULT_API_BASE);
        account.set_api_base(" https://api.wundur.ai/ ");
        assert_eq!(account.api_base(), "https://api.wundur.ai");
        account.set_api_base("");
        assert_eq!(account.api_base(), DEFAULT_API_BASE);
        assert_eq!(Config { api_base: "  ".into(), key_id: String::new() }.api_base(), DEFAULT_API_BASE);
    }

    /// The loopback listener over real sockets: a stray request and a wrong state are refused,
    /// the right one gives the code.
    #[test]
    fn the_listener_waits_for_the_right_redirect() {
        let account = Account::new(Endpoints::default(), Box::new(MemoryStore::default()));
        let sign_in = account.begin_sign_in().unwrap();
        let (port, state) = (sign_in.port(), sign_in.state.clone());
        assert!(sign_in.url.contains(&format!("redirect_uri=http%3A%2F%2Flocalhost%3A{port}%2F")));
        let get = move |target: String| {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(s, "GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut reply = String::new();
            s.read_to_string(&mut reply).unwrap();
            reply
        };
        let client = std::thread::spawn(move || {
            let stray = get("/favicon.ico".into());
            let wrong = get("/?code=evil&state=nope".into());
            let right = get(format!("/?code=the-code&state={state}"));
            (stray, wrong, right)
        });
        let code = sign_in.wait_for_code(Duration::from_secs(10)).unwrap();
        let (stray, wrong, right) = client.join().unwrap();
        assert_eq!(code, "the-code");
        assert!(stray.starts_with("HTTP/1.1 404"));
        assert!(wrong.starts_with("HTTP/1.1 400"));
        assert!(right.starts_with("HTTP/1.1 200") && right.contains("You can close this tab"));
    }

    #[test]
    fn the_listener_can_be_cancelled_and_times_out() {
        let account = Account::new(Endpoints::default(), Box::new(MemoryStore::default()));
        let sign_in = account.begin_sign_in().unwrap();
        assert_eq!(sign_in.wait_for_code(Duration::from_millis(100)), Err(Error::TimedOut));
        sign_in.canceller().cancel();
        assert_eq!(sign_in.wait_for_code(Duration::from_secs(10)), Err(Error::Cancelled));
    }
}
