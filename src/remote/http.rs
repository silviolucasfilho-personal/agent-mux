//! The little HTTP/1.1 the remote control needs: parse a request head with
//! `httparse`, build a response, and nothing else. Three routes, GET only,
//! `Connection: close` everywhere except the WebSocket upgrade.

use std::borrow::Cow;

/// Largest request head we will read before giving up (413).
pub const MAX_HEAD_BYTES: usize = 8 * 1024;
const MAX_HEADERS: usize = 32;

/// A parsed request head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// Path with the query string removed.
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
}

/// Outcome of parsing whatever bytes have arrived so far.
#[derive(Debug)]
pub enum Parsed {
    Complete { req: Request, consumed: usize },
    Partial,
    Invalid,
}

/// Parses a request head. `Partial` means "read more and call again".
pub fn parse_request(buf: &[u8]) -> Parsed {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut req = httparse::Request::new(&mut headers);
    match req.parse(buf) {
        Ok(httparse::Status::Complete(consumed)) => {
            let (method, target) = match (req.method, req.path) {
                (Some(m), Some(p)) => (m.to_string(), p.to_string()),
                _ => return Parsed::Invalid,
            };
            let (path, query) = split_target(&target);
            Parsed::Complete {
                req: Request {
                    method,
                    path,
                    query,
                    headers: req
                        .headers
                        .iter()
                        .map(|h| {
                            (
                                h.name.to_ascii_lowercase(),
                                String::from_utf8_lossy(h.value).trim().to_string(),
                            )
                        })
                        .collect(),
                },
                consumed,
            }
        }
        Ok(httparse::Status::Partial) => Parsed::Partial,
        Err(_) => Parsed::Invalid,
    }
}

fn split_target(target: &str) -> (String, Vec<(String, String)>) {
    match target.split_once('?') {
        None => (percent_decode(target), Vec::new()),
        Some((path, qs)) => {
            let params = qs
                .split('&')
                .filter(|p| !p.is_empty())
                .map(|pair| match pair.split_once('=') {
                    Some((k, v)) => (percent_decode(k), percent_decode(v)),
                    None => (percent_decode(pair), String::new()),
                })
                .collect();
            (percent_decode(path), params)
        }
    }
}

impl Request {
    /// Header lookup; names were lowercased at parse time.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    /// True when a comma-separated header lists `token`, case-insensitively
    /// (`Connection: keep-alive, Upgrade`).
    pub fn header_has_token(&self, name: &str, token: &str) -> bool {
        self.header(name).is_some_and(|v| {
            v.split(',')
                .any(|part| part.trim().eq_ignore_ascii_case(token))
        })
    }

    pub fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// One cookie out of the `Cookie` header.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        let raw = self.header("cookie")?;
        raw.split(';').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k.trim() == name).then(|| v.trim())
        })
    }

    /// The host without its port, lowercased -- for the Origin check.
    pub fn host_only(&self) -> Option<String> {
        host_of(self.header("host")?)
    }
}

/// Strips a scheme and a port: `http://Phone.local:7681` -> `phone.local`.
pub fn host_of(value: &str) -> Option<String> {
    let v = value.trim();
    let v = v.split_once("://").map_or(v, |(_, rest)| rest);
    let v = v.split('/').next().unwrap_or(v);
    if v.is_empty() {
        return None;
    }
    // An IPv6 literal keeps its brackets; anything else drops a :port.
    let host = if let Some(end) = v.strip_prefix('[').and_then(|r| r.find(']')) {
        &v[..end + 2]
    } else {
        v.split(':').next().unwrap_or(v)
    };
    Some(host.to_ascii_lowercase())
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A response to write and close.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Cow<'static, [u8]>,
}

impl Response {
    pub fn new(status: u16) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: Cow::Borrowed(b""),
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Response {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn body(mut self, content_type: &str, body: impl Into<Cow<'static, [u8]>>) -> Response {
        self.body = body.into();
        self.header("Content-Type", content_type)
    }

    pub fn text(status: u16, text: &str) -> Response {
        Response::new(status).body(
            "text/plain; charset=utf-8",
            Cow::Owned(text.as_bytes().to_vec()),
        )
    }

    /// The 101 that completes a WebSocket handshake.
    pub fn switching_protocols(accept: &str) -> Response {
        Response::new(101)
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Accept", accept)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status));
        for (k, v) in &self.headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        // 101 and 304 carry no body, and a 304 with a Content-Length that
        // does not match the cached entity confuses caches.
        if self.status != 101 && self.status != 304 {
            out.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
            out.push_str("Connection: close\r\n");
        }
        out.push_str("\r\n");
        let mut bytes = out.into_bytes();
        if self.status != 101 && self.status != 304 {
            bytes.extend_from_slice(&self.body);
        }
        bytes
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Content Too Large",
        426 => "Upgrade Required",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(raw: &str) -> Request {
        match parse_request(raw.as_bytes()) {
            Parsed::Complete { req, .. } => req,
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn parses_method_path_query_and_headers() {
        let req = complete(
            "GET /?token=abc%2Fdef&x=1 HTTP/1.1\r\nHost: 127.0.0.1:7681\r\nCookie: a=1; amx_token=tok\r\n\r\n",
        );
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/");
        assert_eq!(req.query("token").unwrap(), "abc/def");
        assert_eq!(req.query("x").unwrap(), "1");
        assert_eq!(req.header("host").unwrap(), "127.0.0.1:7681");
        assert_eq!(req.cookie("amx_token").unwrap(), "tok");
        assert_eq!(req.cookie("missing"), None);
    }

    #[test]
    fn connection_header_is_a_token_list() {
        let req = complete("GET /ws HTTP/1.1\r\nConnection: keep-alive, Upgrade\r\n\r\n");
        assert!(req.header_has_token("connection", "upgrade"));
        assert!(!req.header_has_token("connection", "close"));
    }

    #[test]
    fn a_truncated_head_is_partial_not_invalid() {
        assert!(matches!(
            parse_request(b"GET / HTTP/1.1\r\nHost: x\r\n"),
            Parsed::Partial
        ));
        assert!(matches!(
            parse_request(b"not http at all\r\n\r\n"),
            Parsed::Invalid
        ));
    }

    #[test]
    fn host_of_drops_scheme_and_port_and_keeps_ipv6() {
        assert_eq!(
            host_of("http://Phone.local:7681").as_deref(),
            Some("phone.local")
        );
        assert_eq!(host_of("127.0.0.1:7681").as_deref(), Some("127.0.0.1"));
        assert_eq!(host_of("http://[::1]:7681").as_deref(), Some("[::1]"));
        assert_eq!(host_of(""), None);
    }

    #[test]
    fn response_serializes_with_length_and_close() {
        let bytes = Response::new(401)
            .body("text/html", Cow::Borrowed(&b"hi"[..]))
            .to_bytes();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
        assert!(text.contains("Content-Length: 2\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.ends_with("\r\n\r\nhi"));
    }

    #[test]
    fn upgrade_response_has_no_body_or_length() {
        let text = String::from_utf8(Response::switching_protocols("abc").to_bytes()).unwrap();
        assert!(text.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
        assert!(text.contains("Sec-WebSocket-Accept: abc\r\n"));
        assert!(!text.contains("Content-Length"));
        assert!(text.ends_with("\r\n\r\n"));
    }
}
