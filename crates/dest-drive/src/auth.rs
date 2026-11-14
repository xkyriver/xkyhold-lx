//! Google sign-in for an installed app: the system browser, PKCE and a
//! redirect to 127.0.0.1 on a free port, with only the `drive.file` scope.
//! The refresh token goes to the Secret Service; access tokens live in
//! memory and are renewed before they expire, so a backup can run for
//! hours.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use xkyhold_core::{Error, Result};

use crate::http::{Http, Request};

pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";

/// The OAuth client of type "Desktop app" in the `xkyhold` Google Cloud
/// project. Built in from the environment at compile time, or read from it
/// at run time; never in git. For installed apps the secret is not secret
/// in the usual sense (Google documents this), but it stays out of the
/// repository.
#[derive(Clone)]
pub struct Client {
    pub id: String,
    pub secret: String,
}

impl Client {
    pub fn from_env() -> Option<Self> {
        let id = std::env::var("XKYHOLD_LX_CLIENT_ID").ok().or(option_env!("XKYHOLD_LX_CLIENT_ID").map(String::from))?;
        let secret = std::env::var("XKYHOLD_LX_CLIENT_SECRET")
            .ok()
            .or(option_env!("XKYHOLD_LX_CLIENT_SECRET").map(String::from))?;
        Some(Self { id, secret })
    }
}

pub(crate) fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn dec(s: &str) -> String {
    let bytes = s.replace('+', " ").into_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| dec(v)))
}

fn random_b64(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).expect("system randomness");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn form(fields: &[(&str, &str)]) -> Vec<u8> {
    fields.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect::<Vec<_>>().join("&").into_bytes()
}

/// Posts to the token endpoint. `invalid_grant` means the refresh token
/// was revoked or expired: the user must sign in again.
fn token_request(http: &dyn Http, fields: &[(&str, &str)]) -> Result<serde_json::Value> {
    let req =
        Request::new("POST", TOKEN_URL).header("Content-Type", "application/x-www-form-urlencoded").body(form(fields));
    let res = http.send(req).map_err(|e| Error::Network(e.0))?;
    let json = res.json();
    match res.status {
        200 => Ok(json),
        400 | 401 if json["error"] == "invalid_grant" => Err(Error::Session("sign in to Google again".into())),
        s if s >= 500 || s == 429 => Err(Error::Network(format!("Google answered {s}"))),
        s => Err(Error::Refused(format!("Google sign-in answered {s}: {}", json["error"]))),
    }
}

/// The result of signing in: keep `refresh` in the Secret Service.
pub struct SignedIn {
    pub refresh: String,
    pub access: String,
    pub expires_in: u64,
}

/// Signs in through the system browser. `show` receives the sign-in URL
/// (to open it, or to print it); the call waits for the browser to come
/// back to 127.0.0.1, with no time limit.
pub fn sign_in(http: &dyn Http, client: &Client, show: &mut dyn FnMut(&str)) -> Result<SignedIn> {
    let verifier = random_b64(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_b64(16);
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let redirect = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let url = format!(
        "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&code_challenge={challenge}\
         &code_challenge_method=S256&state={state}&access_type=offline&prompt=consent",
        enc(&client.id),
        enc(&redirect),
        enc(SCOPE),
    );
    show(&url);
    loop {
        let (mut stream, _) = listener.accept()?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        let query = line.split_whitespace().nth(1).and_then(|p| p.split_once('?')).map(|(_, q)| q).unwrap_or("");
        // Browsers also ask for /favicon.ico; only the redirect counts.
        if param(query, "state").as_deref() != Some(state.as_str()) {
            let _ = write!(stream, "HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n");
            continue;
        }
        let ok = param(query, "code");
        let page = if ok.is_some() {
            "XkyHold-lx is connected to your Google Drive. You can close this tab."
        } else {
            "Google sign-in was cancelled. You can close this tab."
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<!doctype html><title>XkyHold-lx</title><p style=\"font-family:system-ui\">{page}</p>"
        );
        let code = ok.ok_or_else(|| Error::Session(param(query, "error").unwrap_or_else(|| "cancelled".into())))?;
        let json = token_request(
            http,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("client_id", &client.id),
                ("client_secret", &client.secret),
                ("code_verifier", &verifier),
                ("redirect_uri", &redirect),
            ],
        )?;
        let refresh = json["refresh_token"].as_str().ok_or_else(|| Error::Refused("no refresh token".into()))?;
        return Ok(SignedIn {
            refresh: refresh.to_owned(),
            access: json["access_token"].as_str().unwrap_or_default().to_owned(),
            expires_in: json["expires_in"].as_u64().unwrap_or(0),
        });
    }
}

/// Access tokens for the Drive client, renewed with the refresh token.
pub trait Tokens: Send + Sync {
    /// A valid access token. `renew` asks for a new one even if the current
    /// one has not expired (after a 401).
    fn token(&self, renew: bool) -> Result<String>;
}

pub struct RefreshingTokens {
    http: Arc<dyn Http>,
    client: Client,
    refresh: String,
    current: Mutex<Option<(String, Instant)>>,
}

impl RefreshingTokens {
    pub fn new(http: Arc<dyn Http>, client: Client, refresh: String) -> Self {
        Self { http, client, refresh, current: Mutex::new(None) }
    }
}

impl Tokens for RefreshingTokens {
    fn token(&self, renew: bool) -> Result<String> {
        let mut current = self.current.lock().unwrap();
        if !renew
            && let Some((token, until)) = current.as_ref()
            && Instant::now() < *until
        {
            return Ok(token.clone());
        }
        let json = token_request(
            &*self.http,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &self.refresh),
                ("client_id", &self.client.id),
                ("client_secret", &self.client.secret),
            ],
        )?;
        let token = json["access_token"].as_str().ok_or_else(|| Error::Refused("no access token".into()))?.to_owned();
        // Renew a minute early, so no request starts with a token about to die.
        let life = json["expires_in"].as_u64().unwrap_or(3600).saturating_sub(60);
        *current = Some((token.clone(), Instant::now() + Duration::from_secs(life)));
        if std::env::var_os("XKYHOLD_LX_LOG").is_some() {
            eprintln!(
                "[{}] new Google access token ({}), valid for {} s",
                xkyhold_core::time::now_iso(),
                if renew { "after a 401" } else { "the previous one was due" },
                life + 60
            );
        }
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parameters_are_decoded() {
        assert_eq!(param("a=1&code=4%2F0Ab%2Bc&state=x", "code").as_deref(), Some("4/0Ab+c"));
        assert_eq!(param("a=1", "code"), None);
        assert_eq!(enc("a b/c"), "a%20b%2Fc");
    }
}
