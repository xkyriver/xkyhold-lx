//! A minimal HTTP interface, so the Drive client can be tested against a
//! fake Drive in memory.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn new(method: &'static str, url: impl Into<String>) -> Self {
        Self { method, url: url.into(), headers: Vec::new(), body: Vec::new() }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    pub fn json(self, value: &serde_json::Value) -> Self {
        self.header("Content-Type", "application/json; charset=UTF-8").body(value.to_string().into_bytes())
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

/// The transport failed before an HTTP answer (offline, DNS, reset).
#[derive(Debug)]
pub struct Unreachable(pub String);

pub trait Http: Send + Sync {
    fn send(&self, request: Request) -> Result<Response, Unreachable>;
}

/// The real network, through ureq (rustls).
pub struct Ureq {
    agent: ureq::Agent,
}

impl Default for Ureq {
    fn default() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // Drive answers 308 "Resume Incomplete" to resumable uploads:
            // a status to read, not a redirect to follow.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(300)))
            .user_agent("xkyhold-lx")
            .build();
        Self { agent: config.into() }
    }
}

impl Http for Ureq {
    fn send(&self, request: Request) -> Result<Response, Unreachable> {
        let mut builder = ureq::http::Request::builder().method(request.method).uri(&request.url);
        for (k, v) in &request.headers {
            builder = builder.header(k, v);
        }
        let req = builder.body(request.body).map_err(|e| Unreachable(e.to_string()))?;
        let mut res = self.agent.run(req).map_err(|e| Unreachable(e.to_string()))?;
        let status = res.status().as_u16();
        let headers = res
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or_default().to_owned()))
            .collect();
        let body = res
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| Unreachable(e.to_string()))?;
        Ok(Response { status, headers, body })
    }
}
