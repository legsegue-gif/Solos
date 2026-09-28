//! A one-request HTTP/1.1 client for `127.0.0.1` and nowhere else.
//!
//! Hand-written because the alternative is dragging a client library, and
//! with it TLS, through a musl cross-compile into a binary that ships inside
//! the app. The narrowness is the safety: one connection, one
//! request, `Connection: close`, and a server we wrote on the other end.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// `base` is like `http://127.0.0.1:54321`; `path` starts with `/`.
/// `session` says which conversation the caller runs in (empty for none):
/// a tool that keeps per-conversation state, like background jobs, needs it.
pub fn request(
    base: &str,
    method: &str,
    path: &str,
    token: &str,
    session: &str,
    body: Option<&str>,
) -> Result<Response, String> {
    let hostport = base
        .strip_prefix("http://")
        .ok_or_else(|| format!("expected an http:// address, got {base}"))?
        .trim_end_matches('/');

    let mut stream = TcpStream::connect(hostport)
        .map_err(|e| format!("cannot reach the host at {hostport}: {e}"))?;
    // A device call can wait on a permission prompt the user has to answer,
    // so the read timeout is generous; connecting, on loopback, is not.
    //
    // Best-effort on purpose. iSH's emulated `setsockopt` rejects `SO_RCVTIMEO`
    // with EINVAL, and treating that as fatal made every call fail with a bare
    // "Invalid argument (os error 22)" on a connection that had already
    // succeeded. A missing timeout means a hung host blocks the script; a
    // refused timeout meant nothing worked at all.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let mut req = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {hostport}\r\n\
         Authorization: Bearer {token}\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n"
    );
    // Header values cannot carry control characters; a session id is hex, but
    // the environment is not ours to trust blindly.
    if !session.is_empty() && session.chars().all(|c| c.is_ascii_graphic()) {
        req.push_str(&format!("X-Solos-Session: {session}\r\n"));
    }
    if let Some(b) = body {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }

    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("cannot send the request: {e}"))?;
    stream.flush().map_err(|e| format!("cannot flush the request: {e}"))?;

    // `Connection: close` means the server closes when it is done, so reading
    // to EOF is the framing. No chunked decoding, no keep-alive bookkeeping.
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("cannot read the reply: {e}"))?;
    let text = String::from_utf8_lossy(&raw).into_owned();

    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "the reply had no header/body separator".to_string())?;
    let status_line = head.lines().next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("cannot read a status from {status_line:?}"))?;

    Ok(Response { status, body: body.to_string() })
}

/// Percent-encode a query-string value. Only the characters that would change
/// the meaning of the URL are touched; a guest path is mostly unreserved.
pub fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_encoding_keeps_paths_readable_and_escapes_the_rest() {
        assert_eq!(encode_query("/solos/ws/report.png"), "/solos/ws/report.png");
        assert_eq!(encode_query("/solos/ws/a b"), "/solos/ws/a%20b");
        assert_eq!(encode_query("a&b=c"), "a%26b%3Dc");
        assert_eq!(encode_query("蟋"), "%E8%9F%8B");
    }

    #[test]
    fn a_base_without_the_scheme_is_refused_rather_than_guessed() {
        let e = request("127.0.0.1:1", "GET", "/v1/tools", "t", "", None).unwrap_err();
        assert!(e.contains("expected an http://"), "{e}");
    }
}
