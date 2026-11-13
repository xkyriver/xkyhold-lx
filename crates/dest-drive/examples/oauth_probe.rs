//! Phase 0 prototype (a), throwaway. Answers three questions:
//!
//! 1. Does an installed-app ("Desktop app") OAuth client, with PKCE and a
//!    loopback redirect, sign in with only the `drive.file` scope?
//! 2. Does it get a refresh token, and can it use it?
//! 3. With `drive.file`, does it see the files XkyHold web created (same
//!    Google Cloud project, different client), and the reverse?
//!
//! Usage (client id and secret from the environment, never in git):
//!
//!   XKYHOLD_LX_CLIENT_ID=… XKYHOLD_LX_CLIENT_SECRET=… \
//!     cargo run -p xkyhold-dest-drive --example oauth_probe [-- --create "XkyHold-lx test probe"]
//!
//! It prints no token. With `--create NAME` it creates the empty folder
//! `XkyHold/NAME`, to be looked for from XkyHold web. It never deletes.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest, Sha256};

const AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN: &str = "https://oauth2.googleapis.com/token";
const API: &str = "https://www.googleapis.com/drive/v3";
const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const FOLDER: &str = "application/vnd.google-apps.folder";

fn random(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).expect("system randomness");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| {
            let bytes = v.replace('+', " ").into_bytes();
            let mut out = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'%' && i + 2 < bytes.len() {
                    out.push(u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap(), 16).unwrap());
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).unwrap()
        })
    })
}

fn token(form: &[(&str, &str)]) -> Value {
    let body = form.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect::<Vec<_>>().join("&");
    let mut res = ureq::post(TOKEN)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .config()
        .http_status_as_error(false)
        .build()
        .send(body)
        .expect("token request");
    let status = res.status();
    let json: Value = res.body_mut().read_json().expect("token JSON");
    if !status.is_success() {
        panic!("token endpoint said {status}: {}", json["error"]);
    }
    json
}

fn get(access: &str, url: &str) -> Value {
    ureq::get(url)
        .header("Authorization", &format!("Bearer {access}"))
        .call()
        .expect("Drive request")
        .body_mut()
        .read_json()
        .expect("Drive JSON")
}

fn main() {
    let client_id = std::env::var("XKYHOLD_LX_CLIENT_ID").expect("XKYHOLD_LX_CLIENT_ID");
    let client_secret = std::env::var("XKYHOLD_LX_CLIENT_SECRET").expect("XKYHOLD_LX_CLIENT_SECRET");
    let args: Vec<String> = std::env::args().collect();
    let create = args.iter().position(|a| a == "--create").and_then(|i| args.get(i + 1)).cloned();
    if let Some(name) = &create {
        assert!(name.starts_with("XkyHold-lx test"), "test folders must be named \"XkyHold-lx test…\"");
    }

    // 1. PKCE + loopback redirect on a free port.
    let verifier = random(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random(16);
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback port");
    let redirect = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let url = format!(
        "{AUTH}?client_id={}&redirect_uri={}&response_type=code&scope={}&code_challenge={challenge}\
         &code_challenge_method=S256&state={state}&access_type=offline&prompt=consent",
        enc(&client_id),
        enc(&redirect),
        enc(SCOPE),
    );
    // The URL holds no secret (client id, PKCE challenge, state). With
    // PROBE_NO_OPEN set it is only printed, to open in a chosen browser.
    println!("Sign-in URL: {url}");
    if std::env::var_os("PROBE_NO_OPEN").is_none() {
        let _ = std::process::Command::new("xdg-open").arg(&url).status();
    }

    let (mut stream, _) = listener.accept().expect("redirect");
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).unwrap();
    let query = line.split_whitespace().nth(1).and_then(|p| p.split_once('?')).map(|(_, q)| q).unwrap_or("");
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n\
         <p>XkyHold-lx probe: you can close this tab.</p>"
    );
    assert_eq!(query_param(query, "state").as_deref(), Some(state.as_str()), "state mismatch");
    if let Some(err) = query_param(query, "error") {
        panic!("Google said: {err}");
    }
    let code = query_param(query, "code").expect("authorization code");

    // 2. Code exchange, then a refresh with the refresh token.
    let first = token(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
        ("code_verifier", &verifier),
        ("redirect_uri", &redirect),
    ]);
    println!("Scopes granted: {}", first["scope"]);
    let refresh = first["refresh_token"].as_str().expect("no refresh token in the response");
    println!("Refresh token: received (not shown).");
    let second = token(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
    ]);
    let access = second["access_token"].as_str().expect("access token after refresh");
    println!("Refresh: new access token received, valid for {} s.", second["expires_in"]);

    // 3. What does drive.file let this client see?
    let about = get(access, &format!("{API}/about?fields=user(emailAddress)"));
    println!("Signed in as: {}", about["user"]["emailAddress"]);
    let q = format!("name = 'XkyHold' and mimeType = '{FOLDER}' and 'root' in parents and trashed = false");
    let found = get(access, &format!("{API}/files?q={}&fields=files(id,name,createdTime)", enc(&q)));
    let folders = found["files"].as_array().cloned().unwrap_or_default();
    println!("Visible 'XkyHold' folders in the Drive root: {}", folders.len());
    for folder in &folders {
        let id = folder["id"].as_str().unwrap();
        let q = format!("'{id}' in parents and trashed = false");
        let children = get(access, &format!("{API}/files?q={}&fields=files(name,mimeType,createdTime)", enc(&q)));
        for child in children["files"].as_array().unwrap() {
            println!("  XkyHold/{}  (created {})", child["name"].as_str().unwrap(), child["createdTime"]);
        }
    }

    if let Some(name) = create {
        let parent = match folders.first() {
            Some(f) => f["id"].as_str().unwrap().to_owned(),
            None => create_folder(access, "XkyHold", "root"),
        };
        create_folder(access, &name, &parent);
        println!("Created XkyHold/{name}. Look for it in XkyHold web.");
    }
}

fn create_folder(access: &str, name: &str, parent: &str) -> String {
    let created: Value = ureq::post(&format!("{API}/files?fields=id"))
        .header("Authorization", &format!("Bearer {access}"))
        .send_json(serde_json::json!({ "name": name, "mimeType": FOLDER, "parents": [parent] }))
        .expect("create folder")
        .body_mut()
        .read_json()
        .unwrap();
    created["id"].as_str().unwrap().to_owned()
}
