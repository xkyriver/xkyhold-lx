//! A fake Google Drive in memory, speaking just enough of the HTTP API
//! (and of the token endpoint) for the client's tests. It can fail
//! requests, drop a resumable upload half way, expire access tokens and
//! lag its listings, like the real one does.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::drive::{API, FOLDER_MIME, UPLOAD_API};
use crate::http::{Http, Request, Response, Unreachable};

#[derive(Clone, Debug)]
pub struct FakeFile {
    pub id: String,
    pub name: String,
    pub parent: String,
    pub mime: String,
    pub data: Vec<u8>,
    pub trashed: bool,
}

#[derive(Default)]
pub struct FakeState {
    pub files: Vec<FakeFile>,
    next_id: u64,
    sessions: HashMap<String, (String, String, usize, Vec<u8>)>,
    /// Requests left to answer with 503.
    pub fail_next: usize,
    /// Drop the connection on the next resumable chunk.
    pub drop_next_chunk: bool,
    /// The access token currently valid; others get 401.
    pub valid_token: String,
    pub token_count: u64,
    pub requests: u64,
    pub refresh_ok: bool,
}

pub struct FakeDrive(pub Mutex<FakeState>);

impl Default for FakeDrive {
    fn default() -> Self {
        Self(Mutex::new(FakeState { valid_token: "t0".into(), refresh_ok: true, ..FakeState::default() }))
    }
}

fn reply(status: u16, body: Value) -> Response {
    Response {
        status,
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: body.to_string().into_bytes(),
    }
}

fn query(url: &str) -> HashMap<String, String> {
    url.split_once('?')
        .map(|(_, q)| {
            q.split('&')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| {
                    let mut out = Vec::new();
                    let b = v.as_bytes();
                    let mut i = 0;
                    while i < b.len() {
                        if b[i] == b'%' && i + 2 < b.len() {
                            out.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap(), 16).unwrap());
                            i += 3;
                        } else {
                            out.push(b[i]);
                            i += 1;
                        }
                    }
                    (k.to_owned(), String::from_utf8(out).unwrap())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn quoted_after<'a>(q: &'a str, marker: &str) -> Option<&'a str> {
    let start = q.find(marker)? + marker.len();
    let rest = &q[start..];
    let end = rest.find('\'')?;
    Some(&rest[..end])
}

fn file_json(f: &FakeFile) -> Value {
    let mut v = json!({ "id": f.id, "name": f.name, "mimeType": f.mime });
    if f.mime != FOLDER_MIME {
        v["size"] = json!(f.data.len().to_string());
        v["sha256Checksum"] = json!(hex(&Sha256::digest(&f.data)));
    }
    v
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl FakeState {
    fn create(&mut self, name: &str, parent: &str, mime: &str, data: Vec<u8>) -> FakeFile {
        self.next_id += 1;
        let f = FakeFile {
            id: format!("id{}", self.next_id),
            name: name.into(),
            parent: parent.into(),
            mime: mime.into(),
            data,
            trashed: false,
        };
        self.files.push(f.clone());
        f
    }

    /// Files (not trashed) in a folder, as the API lists them.
    pub fn children(&self, parent: &str) -> Vec<&FakeFile> {
        self.files.iter().filter(|f| f.parent == parent && !f.trashed).collect()
    }

    pub fn find_path(&self, path: &[&str]) -> Option<&FakeFile> {
        let mut parent = "root".to_owned();
        let mut found = None;
        for name in path {
            found = self.children(&parent).into_iter().find(|f| f.name == *name);
            parent = found?.id.clone();
        }
        found
    }
}

impl Http for FakeDrive {
    fn send(&self, req: Request) -> Result<Response, Unreachable> {
        let mut s = self.0.lock().unwrap();
        s.requests += 1;
        let url = req.url.clone();
        if url.starts_with(crate::auth::TOKEN_URL) {
            let body = String::from_utf8_lossy(&req.body).into_owned();
            if body.contains("grant_type=refresh_token") && !s.refresh_ok {
                return Ok(reply(400, json!({ "error": "invalid_grant" })));
            }
            s.token_count += 1;
            s.valid_token = format!("t{}", s.token_count);
            return Ok(reply(200, json!({ "access_token": s.valid_token, "expires_in": 3599, "refresh_token": "r" })));
        }
        if s.fail_next > 0 {
            s.fail_next -= 1;
            return Ok(reply(503, json!({ "error": { "message": "backend error" } })));
        }
        if let Some(session) = url.strip_prefix("fake://session/") {
            let session = session.to_owned();
            let range = req.headers.iter().find(|(k, _)| k == "Content-Range").map(|(_, v)| v.clone()).unwrap();
            let drop = std::mem::take(&mut s.drop_next_chunk) && !req.body.is_empty();
            let (name, parent, size, mut got) = s.sessions.get(&session).cloned().unwrap();
            if let Some(spec) = range.strip_prefix("bytes ").filter(|r| !r.starts_with('*')) {
                let start: usize = spec.split('-').next().unwrap().parse().unwrap();
                if start == got.len() {
                    // A dropped connection still delivers part of the chunk.
                    let keep = if drop { req.body.len() / 2 / (256 * 1024) * (256 * 1024) } else { req.body.len() };
                    got.extend_from_slice(&req.body[..keep]);
                    s.sessions.insert(session.clone(), (name.clone(), parent.clone(), size, got.clone()));
                }
                if drop {
                    return Err(Unreachable("connection reset".into()));
                }
            }
            if got.len() == size {
                s.sessions.remove(&session);
                let f = s.create(&name, &parent, "application/octet-stream", got);
                return Ok(reply(200, file_json(&f)));
            }
            let mut res = reply(308, Value::Null);
            if !got.is_empty() {
                res.headers.push(("Range".into(), format!("bytes=0-{}", got.len() - 1)));
            }
            return Ok(res);
        }
        let auth = req.headers.iter().find(|(k, _)| k == "Authorization").map(|(_, v)| v.clone()).unwrap_or_default();
        if auth != format!("Bearer {}", s.valid_token) {
            return Ok(reply(401, json!({ "error": { "message": "Invalid Credentials" } })));
        }
        let q = query(&url);
        let path = url.split('?').next().unwrap();
        match (req.method, path) {
            ("GET", p) if p == format!("{API}/about") => {
                Ok(reply(200, json!({ "user": { "emailAddress": "test@example.org" } })))
            }
            ("GET", p) if p == format!("{API}/files") => {
                let qs = q.get("q").cloned().unwrap_or_default();
                let parent = quoted_after(&qs, "").and_then(|_| {
                    let idx = qs.find(" in parents")?;
                    let before = &qs[..idx];
                    let open = before[..before.len() - 1].rfind('\'')?;
                    Some(before[open + 1..before.len() - 1].to_owned())
                });
                let parent = parent.unwrap_or_else(|| "root".into());
                let name = qs.starts_with("name = ").then(|| quoted_after(&qs, "name = '").unwrap().to_owned());
                let folders_only = qs.contains("mimeType = ");
                let files: Vec<Value> = s
                    .children(&parent)
                    .into_iter()
                    .filter(|f| name.as_ref().is_none_or(|n| *n == f.name))
                    .filter(|f| !folders_only || f.mime == FOLDER_MIME)
                    .map(file_json)
                    .collect();
                // Two pages when there are many, to exercise paging.
                let page: usize = q.get("pageToken").map_or(0, |p| p.parse().unwrap());
                let per = 3;
                let chunk: Vec<Value> = files.iter().skip(page * per).take(per).cloned().collect();
                let mut body = json!({ "files": chunk });
                if files.len() > (page + 1) * per {
                    body["nextPageToken"] = json!((page + 1).to_string());
                }
                Ok(reply(200, body))
            }
            ("POST", p) if p == format!("{API}/files") => {
                let meta: Value = serde_json::from_slice(&req.body).unwrap();
                let f = s.create(
                    meta["name"].as_str().unwrap(),
                    meta["parents"][0].as_str().unwrap(),
                    FOLDER_MIME,
                    Vec::new(),
                );
                Ok(reply(200, json!({ "id": f.id })))
            }
            ("PATCH", p) if p.starts_with(&format!("{API}/files/")) => {
                let id = &p[format!("{API}/files/").len()..];
                let f = s.files.iter_mut().find(|f| f.id == id).unwrap();
                f.trashed = true;
                Ok(reply(200, json!({})))
            }
            ("GET", p) if p.starts_with(&format!("{API}/files/")) => {
                let id = &p[format!("{API}/files/").len()..];
                let Some(f) = s.files.iter().find(|f| f.id == id && !f.trashed) else {
                    return Ok(reply(404, json!({ "error": { "message": "File not found" } })));
                };
                let range = req.headers.iter().find(|(k, _)| k == "Range").map(|(_, v)| v.clone());
                Ok(match range.and_then(|r| r.strip_prefix("bytes=").map(String::from)) {
                    Some(r) => {
                        let (a, b) = r.split_once('-').unwrap();
                        let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
                        let end = (b + 1).min(f.data.len());
                        Response { status: 206, headers: vec![], body: f.data[a.min(end)..end].to_vec() }
                    }
                    None => Response { status: 200, headers: vec![], body: f.data.clone() },
                })
            }
            ("POST", p)
                if p == format!("{UPLOAD_API}/files")
                    && q.get("uploadType").map(String::as_str) == Some("multipart") =>
            {
                let ct = req.headers.iter().find(|(k, _)| k == "Content-Type").unwrap().1.clone();
                let boundary = ct.split("boundary=").nth(1).unwrap().to_owned();
                let body = req.body.clone();
                let sep = format!("--{boundary}").into_bytes();
                let parts: Vec<&[u8]> = split(&body, &sep);
                let meta_part = parts[1];
                let meta_start = find(meta_part, b"\r\n\r\n").unwrap() + 4;
                let meta: Value = serde_json::from_slice(trim_crlf(&meta_part[meta_start..])).unwrap();
                let data_part = parts[2];
                let data_start = find(data_part, b"\r\n\r\n").unwrap() + 4;
                let data = data_part[data_start..data_part.len() - 2].to_vec();
                let f = s.create(
                    meta["name"].as_str().unwrap(),
                    meta["parents"][0].as_str().unwrap(),
                    "application/octet-stream",
                    data,
                );
                Ok(reply(200, file_json(&f)))
            }
            ("POST", p) if p == format!("{UPLOAD_API}/files") => {
                let meta: Value = serde_json::from_slice(&req.body).unwrap();
                let size: usize =
                    req.headers.iter().find(|(k, _)| k == "X-Upload-Content-Length").unwrap().1.parse().unwrap();
                let id = format!("s{}", s.sessions.len() + s.files.len());
                s.sessions.insert(
                    id.clone(),
                    (
                        meta["name"].as_str().unwrap().into(),
                        meta["parents"][0].as_str().unwrap().into(),
                        size,
                        Vec::new(),
                    ),
                );
                let mut res = reply(200, Value::Null);
                res.headers.push(("Location".into(), format!("fake://session/{id}")));
                Ok(res)
            }
            ("PATCH", p) if p.starts_with(&format!("{UPLOAD_API}/files/")) => {
                let id = p[format!("{UPLOAD_API}/files/").len()..].to_owned();
                s.files.iter_mut().find(|f| f.id == id).unwrap().data = req.body.clone();
                Ok(reply(200, json!({})))
            }
            other => panic!("fake Drive: unexpected request {other:?}"),
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn split<'a>(hay: &'a [u8], sep: &[u8]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut rest = hay;
    while let Some(i) = find(rest, sep) {
        out.push(&rest[..i]);
        rest = &rest[i + sep.len()..];
    }
    out.push(rest);
    out
}

fn trim_crlf(b: &[u8]) -> &[u8] {
    b.strip_suffix(b"\r\n").unwrap_or(b)
}
