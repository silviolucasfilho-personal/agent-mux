//! A blocking WebSocket client for the remote-control tests.
//!
//! Hand-written on a std `TcpStream` rather than pulled from a crate: the
//! server's framing is hand-written too, and a second implementation that
//! shares no code with it is what makes the round trip worth testing.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// One plain GET, read to completion.
pub fn get(addr: SocketAddr, path: &str) -> std::io::Result<Response> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    Ok(parse_response(&raw))
}

/// A GET with extra header lines (already `\r\n`-free).
pub fn get_with(addr: SocketAddr, path: &str, extra: &[&str]) -> std::io::Result<Response> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut head = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n");
    for line in extra {
        head.push_str(line);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    Ok(parse_response(&raw))
}

fn parse_response(raw: &[u8]) -> Response {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_ref(), ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Response {
        status,
        headers,
        body: body.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Close(u16),
}

pub struct WsClient {
    stream: TcpStream,
    buf: Vec<u8>,
    mask_seed: u32,
}

impl WsClient {
    /// Opens `/ws` and completes the handshake, validating the accept key.
    pub fn connect(addr: SocketAddr, token: &str) -> Result<WsClient, u16> {
        Self::connect_with(addr, &[&format!("Cookie: amx_token={token}")])
    }

    pub fn connect_with(addr: SocketAddr, extra: &[&str]) -> Result<WsClient, u16> {
        let mut stream = TcpStream::connect(addr).map_err(|_| 0u16)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|_| 0u16)?;
        // The RFC 6455 section 1.3 example key, so the expected accept value
        // is a known constant.
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let mut head = format!(
            "GET /ws HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n"
        );
        for line in extra {
            head.push_str(line);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).map_err(|_| 0u16)?;

        // Read exactly the response head; anything after it is frame data.
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(0) | Err(_) => return Err(0),
                Ok(_) => buf.push(byte[0]),
            }
            if buf.len() > 8192 {
                return Err(0);
            }
        }
        let response = parse_response(&buf);
        if response.status != 101 {
            return Err(response.status);
        }
        assert_eq!(
            response.header("sec-websocket-accept"),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
            "server computed the wrong accept key"
        );
        Ok(WsClient {
            stream,
            buf: Vec::new(),
            mask_seed: 0x1234_5678,
        })
    }

    fn next_mask(&mut self) -> [u8; 4] {
        // Any varying mask exercises the server's unmasking; this is a test
        // client, so a counter is enough and keeps failures reproducible.
        self.mask_seed = self
            .mask_seed
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        self.mask_seed.to_be_bytes()
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) {
        let mask = self.next_mask();
        let mut frame = vec![0x80 | opcode];
        match payload.len() {
            n if n < 126 => frame.push(0x80 | n as u8),
            n if n <= u16::MAX as usize => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        let _ = self.stream.write_all(&frame);
    }

    pub fn send_text(&mut self, text: &str) {
        self.send(0x1, text.as_bytes());
    }

    pub fn send_json(&mut self, value: serde_json::Value) {
        self.send_text(&value.to_string());
    }

    pub fn send_binary(&mut self, bytes: &[u8]) {
        self.send(0x2, bytes);
    }

    /// An input frame: `[0x10][session u32 BE][bytes]`.
    pub fn send_input(&mut self, session: usize, bytes: &[u8]) {
        let mut frame = vec![0x10u8];
        frame.extend_from_slice(&(session as u32).to_be_bytes());
        frame.extend_from_slice(bytes);
        self.send_binary(&frame);
    }

    /// Next message, or `None` on timeout.
    pub fn recv(&mut self, timeout: Duration) -> Option<Msg> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(msg) = self.take_frame() {
                return Some(msg);
            }
            if Instant::now() >= deadline {
                return None;
            }
            let _ = self.stream.set_read_timeout(Some(
                Duration::from_millis(50).min(deadline - Instant::now()),
            ));
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => return None,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(_) => continue,
            }
        }
    }

    /// Next message whose JSON `t` is `tag`, skipping anything else.
    pub fn recv_json(&mut self, tag: &str, timeout: Duration) -> Option<serde_json::Value> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.recv(deadline - Instant::now())? {
                Msg::Text(text) => {
                    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
                    if v["t"] == tag {
                        return Some(v);
                    }
                }
                Msg::Close(_) => return None,
                _ => {}
            }
        }
        None
    }

    /// Next binary frame of `kind` for `session`, with its payload.
    pub fn recv_kind(&mut self, kind: u8, timeout: Duration) -> Option<(usize, Vec<u8>)> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.recv(deadline - Instant::now())? {
                Msg::Binary(frame) if frame.len() >= 5 && frame[0] == kind => {
                    let session =
                        u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
                    return Some((session, frame[5..].to_vec()));
                }
                Msg::Close(_) => return None,
                _ => {}
            }
        }
        None
    }

    /// Accumulates OUTPUT payloads for `session` until `needle` shows up.
    pub fn wait_for_output(&mut self, session: usize, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut seen = String::new();
        while Instant::now() < deadline {
            let Some((sid, payload)) = self.recv_kind(0x01, deadline - Instant::now()) else {
                continue;
            };
            if sid == session {
                seen.push_str(&String::from_utf8_lossy(&payload));
                if seen.contains(needle) {
                    return true;
                }
            }
        }
        false
    }

    fn take_frame(&mut self) -> Option<Msg> {
        let buf = &self.buf;
        if buf.len() < 2 {
            return None;
        }
        let opcode = buf[0] & 0x0F;
        let masked = buf[1] & 0x80 != 0;
        let short = (buf[1] & 0x7F) as usize;
        let mut offset = 2;
        let len = match short {
            126 => {
                if buf.len() < 4 {
                    return None;
                }
                offset += 2;
                u16::from_be_bytes([buf[2], buf[3]]) as usize
            }
            127 => {
                if buf.len() < 10 {
                    return None;
                }
                offset += 8;
                u64::from_be_bytes(buf[2..10].try_into().ok()?) as usize
            }
            n => n,
        };
        if masked {
            offset += 4; // a server must not mask, but do not hang if it does
        }
        if buf.len() < offset + len {
            return None;
        }
        let payload = buf[offset..offset + len].to_vec();
        self.buf.drain(..offset + len);
        Some(match opcode {
            0x1 => Msg::Text(String::from_utf8_lossy(&payload).into_owned()),
            0x2 => Msg::Binary(payload),
            0x9 => Msg::Ping(payload),
            0x8 => Msg::Close(if payload.len() >= 2 {
                u16::from_be_bytes([payload[0], payload[1]])
            } else {
                1000
            }),
            _ => return None,
        })
    }
}
