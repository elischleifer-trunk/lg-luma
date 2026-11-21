//! A minimal client for the TV web engine's DevTools (Chromium's remote debugging protocol).
//!
//! LG's web app manager (WAM) runs every web app in one Chromium and, on the TVs Luma runs on,
//! listens for DevTools on loopback, naming the port in `/var/lib/wam/DevToolsActivePort`.
//! Luma uses it for one thing: giving its Criterion player the Channel's sign-in cookies, which
//! are HttpOnly, so no page script can set them (see `criterion_session`). WAM refuses
//! browser-wide cookie calls, so calls go to a page of the app itself.
//!
//! Plain WebSocket over loopback TCP, text frames only; no TLS and no extensions.
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use serde_json::{Value, json};

const PORT_FILE: &str = "/var/lib/wam/DevToolsActivePort";
const TIMEOUT: Duration = Duration::from_secs(3);
/// DevTools replies for cookie calls are small; anything bigger is not for us.
const MESSAGE_LIMIT: usize = 4 << 20;

pub struct DevTools {
    stream: BufReader<TcpStream>,
    next: u64,
}

fn port() -> Result<u16, String> {
    let text = std::fs::read_to_string(PORT_FILE)
        .map_err(|_| "the TV's web engine offers no DevTools".to_string())?;
    text.lines()
        .next()
        .and_then(|line| line.trim().parse().ok())
        .ok_or_else(|| "the web engine's DevTools port is unreadable".into())
}

fn address(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

impl DevTools {
    /// Connects to the open page of web app `app_id`.
    pub fn page(app_id: &str) -> Result<Self, String> {
        Self::find(app_id, |_| true)
    }

    /// Connects to `app_id`'s own start page (a local file), once it has opened.
    pub fn start_page(app_id: &str) -> Result<Self, String> {
        Self::find(app_id, |url| url.starts_with("file:"))
    }

    fn find(app_id: &str, wanted: impl Fn(&str) -> bool) -> Result<Self, String> {
        let port = port()?;
        let (status, body) = http_get(port, "/json/list")?;
        if status != 200 {
            return Err(format!("DevTools answered HTTP {status}"));
        }
        let pages: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
        let socket = pages
            .as_array()
            .into_iter()
            .flatten()
            .find(|page| {
                page.get("type").and_then(Value::as_str) == Some("page")
                    && page.get("description").and_then(Value::as_str) == Some(app_id)
                    && wanted(page.get("url").and_then(Value::as_str).unwrap_or(""))
            })
            .and_then(|page| page.get("webSocketDebuggerUrl")?.as_str())
            .ok_or_else(|| format!("{app_id} has no open page"))?;
        let path = socket
            .strip_prefix("ws://")
            .and_then(|rest| rest.find('/').map(|slash| &rest[slash..]))
            .ok_or("DevTools gave an unexpected page address")?;
        Self::connect(port, path)
    }

    fn connect(port: u16, path: &str) -> Result<Self, String> {
        let stream =
            TcpStream::connect_timeout(&address(port), TIMEOUT).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(TIMEOUT))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(TIMEOUT))
            .map_err(|e| e.to_string())?;
        let mut key = [0u8; 16];
        getrandom::getrandom(&mut key).map_err(|e| e.to_string())?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\r\n",
            base64(&key)
        );
        let mut reader = BufReader::new(stream);
        reader
            .get_mut()
            .write_all(request.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut status = String::new();
        reader.read_line(&mut status).map_err(|e| e.to_string())?;
        if !status.starts_with("HTTP/1.1 101") {
            return Err(format!(
                "DevTools refused the connection: {}",
                status.trim()
            ));
        }
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("DevTools closed the connection".into());
            }
            if line == "\r\n" {
                break;
            }
        }
        Ok(Self {
            stream: reader,
            next: 0,
        })
    }

    /// Sends `method` and waits for its result; events in between are skipped.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        let message = json!({"id": id, "method": method, "params": params}).to_string();
        self.send(0x1, message.as_bytes())?;
        loop {
            let reply: Value =
                serde_json::from_slice(&self.receive()?).map_err(|e| e.to_string())?;
            if reply.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = reply.get("error") {
                let message = error.get("message").and_then(Value::as_str).unwrap_or("");
                return Err(format!("{method}: {message}"));
            }
            return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mut mask = [0u8; 4];
        getrandom::getrandom(&mut mask).map_err(|e| e.to_string())?;
        let frame = frame(opcode, payload, mask);
        self.stream
            .get_mut()
            .write_all(&frame)
            .map_err(|e| e.to_string())
    }

    /// One whole message, joining continuation frames and answering pings.
    fn receive(&mut self) -> Result<Vec<u8>, String> {
        let mut message = Vec::new();
        loop {
            let mut head = [0u8; 2];
            self.stream
                .read_exact(&mut head)
                .map_err(|e| e.to_string())?;
            let fin = head[0] & 0x80 != 0;
            let opcode = head[0] & 0x0f;
            let masked = head[1] & 0x80 != 0;
            let length = match head[1] & 0x7f {
                126 => {
                    let mut bytes = [0u8; 2];
                    self.stream
                        .read_exact(&mut bytes)
                        .map_err(|e| e.to_string())?;
                    u64::from(u16::from_be_bytes(bytes))
                }
                127 => {
                    let mut bytes = [0u8; 8];
                    self.stream
                        .read_exact(&mut bytes)
                        .map_err(|e| e.to_string())?;
                    u64::from_be_bytes(bytes)
                }
                short => u64::from(short),
            };
            if length as usize > MESSAGE_LIMIT || message.len() + length as usize > MESSAGE_LIMIT {
                return Err("DevTools sent too much".into());
            }
            let mut mask = [0u8; 4];
            if masked {
                self.stream
                    .read_exact(&mut mask)
                    .map_err(|e| e.to_string())?;
            }
            let mut payload = vec![0u8; length as usize];
            self.stream
                .read_exact(&mut payload)
                .map_err(|e| e.to_string())?;
            if masked {
                for (i, byte) in payload.iter_mut().enumerate() {
                    *byte ^= mask[i % 4];
                }
            }
            match opcode {
                0x8 => return Err("DevTools closed the connection".into()),
                0x9 => self.send(0xA, &payload)?,
                0xA => {}
                _ => {
                    message.extend_from_slice(&payload);
                    if fin {
                        return Ok(message);
                    }
                }
            }
        }
    }
}

/// A client frame: final, masked.
fn frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut frame = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => frame.push(0x80 | n as u8),
        n if n <= 0xffff => {
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
    frame
}

fn http_get(port: u16, path: &str) -> Result<(u16, Vec<u8>), String> {
    let mut stream =
        TcpStream::connect_timeout(&address(port), TIMEOUT).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(|e| e.to_string())?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    // WAM keeps the connection open after its reply, so the body is read by its length rather
    // than to the end of the stream.
    let mut reader = BufReader::new(stream.take(MESSAGE_LIMIT as u64));
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| e.to_string())?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or("DevTools sent no HTTP status")?;
    let mut length = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("DevTools ended its reply early".into());
        }
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap_or((&line, ""));
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse::<usize>().ok(),
            "transfer-encoding" => chunked = value.trim().eq_ignore_ascii_case("chunked"),
            _ => {}
        }
    }
    let body = if chunked {
        read_chunked(&mut reader)?
    } else if let Some(length) = length {
        let mut body = vec![0u8; length.min(MESSAGE_LIMIT)];
        reader.read_exact(&mut body).map_err(|e| e.to_string())?;
        body
    } else {
        let mut body = Vec::new();
        reader.read_to_end(&mut body).map_err(|e| e.to_string())?;
        body
    };
    Ok((status, body))
}

/// A chunked body, up to its empty last chunk.
fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "bad chunk size")?;
        if body.len() + size > MESSAGE_LIMIT {
            return Err("DevTools sent too much".into());
        }
        let mut chunk = vec![0u8; size + 2];
        reader.read_exact(&mut chunk).map_err(|e| e.to_string())?;
        if size == 0 {
            return Ok(body);
        }
        body.extend_from_slice(&chunk[..size]);
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn base64_matches_the_rfc_examples() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn frames_are_masked_with_the_right_length_header() {
        let mask = [1, 2, 3, 4];
        let short = frame(0x1, b"hi", mask);
        assert_eq!(&short[..2], &[0x81, 0x82]);
        assert_eq!(&short[2..6], &mask);
        assert_eq!(&short[6..], &[b'h' ^ 1, b'i' ^ 2]);
        let medium = frame(0x1, &[0; 300], mask);
        assert_eq!(&medium[..4], &[0x81, 0x80 | 126, 1, 44]);
        let long = frame(0x1, &vec![0; 70_000], mask);
        assert_eq!(long[1], 0x80 | 127);
        assert_eq!(&long[2..10], &70_000u64.to_be_bytes());
    }

    #[test]
    fn chunked_replies_are_joined() {
        let mut whole = std::io::Cursor::new(&b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"[..]);
        assert_eq!(read_chunked(&mut whole).unwrap(), b"abcde");
        let mut short = std::io::Cursor::new(&b"5\r\nab"[..]);
        assert!(read_chunked(&mut short).is_err());
    }

    /// A fake DevTools endpoint: answers the handshake, a ping and then a reply to call 1 split
    /// across two frames, after an unrelated event.
    #[test]
    fn a_call_skips_events_answers_pings_and_joins_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            let stream = reader.get_mut();
            stream
                .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n")
                .unwrap();
            // Read the client's frame header and payload (masked).
            let mut head = [0u8; 2];
            stream.read_exact(&mut head).unwrap();
            let mut rest = vec![0u8; 4 + (head[1] & 0x7f) as usize];
            stream.read_exact(&mut rest).unwrap();
            let server_frame = |opcode: u8, fin: bool, payload: &[u8]| {
                let mut f = vec![if fin { 0x80 } else { 0 } | opcode, payload.len() as u8];
                f.extend_from_slice(payload);
                f
            };
            stream
                .write_all(&server_frame(0x1, true, br#"{"method":"Network.event"}"#))
                .unwrap();
            stream.write_all(&server_frame(0x9, true, b"p")).unwrap();
            stream
                .write_all(&server_frame(0x1, false, br#"{"id":1,"result":"#))
                .unwrap();
            stream
                .write_all(&server_frame(0x0, true, br#"{"ok":true}}"#))
                .unwrap();
            // The pong for the ping.
            let mut pong = [0u8; 2];
            stream.read_exact(&mut pong).unwrap();
            pong[0]
        });
        let mut devtools = DevTools::connect(port, "/devtools/page/x").unwrap();
        let result = devtools.call("Network.getCookies", json!({})).unwrap();
        assert_eq!(result, json!({"ok": true}));
        assert_eq!(
            server.join().unwrap(),
            0x8A,
            "a final pong answered the ping"
        );
    }
}
