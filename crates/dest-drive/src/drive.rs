//! Google Drive API v3: just what a backup needs (port of XkyHold's
//! `src/storage/drive.ts`). Retries rate limits, server errors and dropped
//! connections; renews the access token once on a 401.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;
use xkyhold_core::{Error, Result};

use crate::auth::{Tokens, enc};
use crate::http::{Http, Request, Response};

pub const API: &str = "https://www.googleapis.com/drive/v3";
pub const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3";
pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
/// Drive requires every chunk but the last to be a multiple of 256 KiB.
pub const CHUNK_UNIT: usize = 256 * 1024;
const UPLOAD_CHUNK: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub sha256_checksum: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn status_error(res: &Response) -> Error {
    let detail = res.json()["error"]["message"].as_str().unwrap_or_default().to_owned();
    let message = if detail.is_empty() { format!("Google Drive answered {}", res.status) } else { detail };
    match res.status {
        401 => Error::Session(message),
        404 => Error::NotFound(message),
        429 | 500.. => Error::Network(message),
        _ => Error::Refused(message),
    }
}

fn transient(err: &Error) -> bool {
    matches!(err, Error::Network(_))
}

/// Keeps uploads under a rate, shared by all upload threads.
struct Limiter {
    bytes_per_sec: u64,
    start: Instant,
    sent: u64,
}

pub struct Drive {
    http: Arc<dyn Http>,
    tokens: Arc<dyn Tokens>,
    /// Waits before each retry; shortened in tests.
    pub delays: Vec<Duration>,
    limiter: Option<Mutex<Limiter>>,
}

impl Drive {
    pub fn new(http: Arc<dyn Http>, tokens: Arc<dyn Tokens>) -> Self {
        let delays = [1, 3, 8, 20, 45].map(Duration::from_secs).to_vec();
        Self { http, tokens, delays, limiter: None }
    }

    /// Limits uploads to about this many bytes per second.
    pub fn limit_uploads(&mut self, bytes_per_sec: u64) {
        self.limiter =
            (bytes_per_sec > 0).then(|| Mutex::new(Limiter { bytes_per_sec, start: Instant::now(), sent: 0 }));
    }

    /// Waits until sending `n` more bytes keeps under the limit.
    fn pace(&self, n: usize) {
        let Some(limiter) = &self.limiter else { return };
        let wait = {
            let mut l = limiter.lock().unwrap();
            l.sent += n as u64;
            let due = Duration::from_secs_f64(l.sent as f64 / l.bytes_per_sec as f64);
            due.saturating_sub(l.start.elapsed())
        };
        std::thread::sleep(wait);
    }

    /// Sends one request with the access token; on a 401, renews the token
    /// once and tries again.
    fn authorized(&self, build: &dyn Fn() -> Request) -> Result<Response> {
        let mut renew = false;
        loop {
            let token = self.tokens.token(renew)?;
            let res = self
                .http
                .send(build().header("Authorization", format!("Bearer {token}")))
                .map_err(|e| Error::Network(e.0))?;
            if res.status == 401 && !renew {
                renew = true;
                continue;
            }
            return Ok(res);
        }
    }

    /// Repeats `work` when Drive is briefly unavailable.
    pub fn retry<T>(&self, mut work: impl FnMut() -> Result<T>) -> Result<T> {
        let mut attempt = 0;
        loop {
            match work() {
                Err(err) if transient(&err) && attempt < self.delays.len() => {
                    std::thread::sleep(self.delays[attempt]);
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    fn call(&self, build: &dyn Fn() -> Request) -> Result<Response> {
        self.retry(|| {
            let res = self.authorized(build)?;
            if (200..300).contains(&res.status) || res.status == 308 { Ok(res) } else { Err(status_error(&res)) }
        })
    }

    pub fn about_email(&self) -> Result<String> {
        let res = self.call(&|| Request::new("GET", format!("{API}/about?fields=user(emailAddress)")))?;
        Ok(res.json()["user"]["emailAddress"].as_str().unwrap_or_default().to_owned())
    }

    /// Finds a folder by name under `parent` (`root` for My Drive), creating it if missing.
    pub fn ensure_folder(&self, name: &str, parent: &str) -> Result<String> {
        let q = format!(
            "name = {} and mimeType = '{FOLDER_MIME}' and {} in parents and trashed = false",
            quote(name),
            quote(parent)
        );
        let res =
            self.call(&|| Request::new("GET", format!("{API}/files?q={}&fields=files(id)&pageSize=1", enc(&q))))?;
        if let Some(id) = res.json()["files"][0]["id"].as_str() {
            return Ok(id.to_owned());
        }
        self.create_folder(name, parent)
    }

    pub fn create_folder(&self, name: &str, parent: &str) -> Result<String> {
        let body = json!({ "name": name, "mimeType": FOLDER_MIME, "parents": [parent] });
        let res = self.call(&|| Request::new("POST", format!("{API}/files?fields=id")).json(&body))?;
        res.json()["id"].as_str().map(String::from).ok_or_else(|| Error::Refused("no folder id".into()))
    }

    /// Every non-trashed child of a folder.
    pub fn list_children(&self, folder: &str) -> Result<Vec<DriveFile>> {
        let q = enc(&format!("{} in parents and trashed = false", quote(folder)));
        let mut out = Vec::new();
        let mut page = String::new();
        loop {
            let url = format!(
                "{API}/files?q={q}&fields=nextPageToken,files(id,name,size,sha256Checksum,mimeType)&pageSize=1000{page}"
            );
            let body = self.call(&|| Request::new("GET", url.clone()))?.json();
            let files: Vec<DriveFile> = serde_json::from_value(body["files"].clone()).unwrap_or_default();
            out.extend(files);
            match body["nextPageToken"].as_str() {
                Some(token) => page = format!("&pageToken={}", enc(token)),
                None => return Ok(out),
            }
        }
    }

    /// Moves a file to the Drive trash. Never deletes permanently.
    pub fn trash(&self, id: &str) -> Result<()> {
        let body = json!({ "trashed": true });
        self.call(&|| Request::new("PATCH", format!("{API}/files/{id}")).json(&body)).map(|_| ())
    }

    /// Downloads a file, or `length` bytes from `offset`.
    pub fn download(&self, id: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>> {
        if let Some((_, 0)) = range {
            return Ok(Vec::new());
        }
        let res = self.call(&|| {
            let req = Request::new("GET", format!("{API}/files/{id}?alt=media"));
            match range {
                Some((offset, length)) => req.header("Range", format!("bytes={offset}-{}", offset + length - 1)),
                None => req,
            }
        })?;
        Ok(match range {
            // A server that ignores Range answers 200 with the whole file.
            Some((offset, length)) if res.status != 206 => {
                let start = (offset as usize).min(res.body.len());
                let end = (start + length as usize).min(res.body.len());
                res.body[start..end].to_vec()
            }
            _ => res.body,
        })
    }

    /// Creates a file in one request. For small content.
    pub fn upload_small(&self, name: &str, parent: &str, data: &[u8]) -> Result<DriveFile> {
        let boundary = format!("xkyhold{}", xkyhold_core::crypto::random_id());
        let meta = json!({ "name": name, "parents": [parent] }).to_string();
        let mut body = format!(
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
        self.pace(data.len());
        let res = self.call(&|| {
            Request::new("POST", format!("{UPLOAD_API}/files?uploadType=multipart&fields=id,name,size"))
                .header("Content-Type", format!("multipart/related; boundary={boundary}"))
                .body(body.clone())
        })?;
        serde_json::from_slice(&res.body).map_err(|e| Error::Refused(e.to_string()))
    }

    /// Replaces the content of an existing file (only `config` changes).
    pub fn replace_content(&self, id: &str, data: &[u8]) -> Result<()> {
        self.call(&|| {
            Request::new("PATCH", format!("{UPLOAD_API}/files/{id}?uploadType=media"))
                .header("Content-Type", "application/octet-stream")
                .body(data.to_vec())
        })
        .map(|_| ())
    }

    /// A resumable upload: opens a session, then sends 8 MiB at a time.
    /// After a failure it asks the server where it stopped and goes on from
    /// there, so an interrupted pack is not sent again from the start.
    pub fn upload_resumable(&self, name: &str, parent: &str, data: &[u8]) -> Result<DriveFile> {
        let meta = json!({ "name": name, "parents": [parent] });
        let size = data.len();
        let res = self.call(&|| {
            Request::new("POST", format!("{UPLOAD_API}/files?uploadType=resumable"))
                .header("X-Upload-Content-Type", "application/octet-stream")
                .header("X-Upload-Content-Length", size.to_string())
                .json(&meta)
        })?;
        let session = res.header("Location").ok_or_else(|| Error::Refused("no upload session".into()))?.to_owned();
        let mut offset = 0usize;
        let mut fresh = true;
        self.retry(|| {
            if !fresh {
                match self.upload_status(&session, size)? {
                    Step::Next(next) => offset = next,
                    Step::Done(file) => return Ok(file),
                }
            }
            fresh = false;
            loop {
                let end = (offset + UPLOAD_CHUNK).min(size);
                let range =
                    if size == 0 { format!("bytes */{size}") } else { format!("bytes {offset}-{}/{size}", end - 1) };
                self.pace(end - offset);
                let res = self
                    .http
                    .send(
                        Request::new("PUT", session.clone())
                            .header("Content-Range", range)
                            .body(data[offset..end].to_vec()),
                    )
                    .map_err(|e| Error::Network(e.0))?;
                match res.status {
                    308 => offset = next_offset(res.header("Range")),
                    200 | 201 => return serde_json::from_slice(&res.body).map_err(|e| Error::Refused(e.to_string())),
                    _ => return Err(status_error(&res)),
                }
            }
        })
    }

    /// Where an interrupted upload stands on the server.
    fn upload_status(&self, session: &str, size: usize) -> Result<Step> {
        let res = self
            .http
            .send(Request::new("PUT", session).header("Content-Range", format!("bytes */{size}")))
            .map_err(|e| Error::Network(e.0))?;
        match res.status {
            308 => Ok(Step::Next(next_offset(res.header("Range")))),
            200 | 201 => serde_json::from_slice(&res.body).map(Step::Done).map_err(|e| Error::Refused(e.to_string())),
            _ => Err(status_error(&res)),
        }
    }
}

enum Step {
    Next(usize),
    Done(DriveFile),
}

/// "bytes=0-524287" -> 524288. No header -> 0.
pub fn next_offset(range: Option<&str>) -> usize {
    range.and_then(|r| r.strip_prefix("bytes=0-")).and_then(|n| n.parse::<usize>().ok()).map_or(0, |n| n + 1)
}
