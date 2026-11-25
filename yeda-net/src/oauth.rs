//! What every OAuth sign-in shares: PKCE, form encoding, the token endpoint's answer, and the
//! loopback listener a browser returns to.

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// A PKCE verifier and its S256 challenge. The spec wants 43-128 characters; one compact UUID is 32.
pub fn pkce() -> (String, String) {
    let id = || uuid::Uuid::new_v4().simple().to_string();
    let verifier = format!("{}{}", id(), id());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// The callback's query parameters, from the request line only: answering before the browser
/// closes the connection keeps it from hanging on a keep-alive.
pub fn callback_query(stream: &mut TcpStream, expected_state: &str) -> Result<std::collections::HashMap<String, String>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) | Err(_) => break,
            Ok(_) if header.trim().is_empty() => break,
            Ok(_) => {}
        }
    }
    let line = request.lines().next().unwrap_or_default();
    let query = line.split_whitespace().nth(1).and_then(|path| path.split_once('?')).map(|(_, q)| q).unwrap_or_default();
    let mut params = std::collections::HashMap::new();
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(k), percent_decode(v));
    }
    if let Some(error) = params.get("error") {
        bail!("the sign-in was refused: {error}");
    }
    if params.get("state").map(String::as_str) != Some(expected_state) {
        bail!("the sign-in callback does not belong to this attempt");
    }
    Ok(params)
}


/// Waits for the browser to reach `listener`, and answers it with `page`.
pub fn wait_for_callback(listener: &TcpListener, timeout: Duration, expected_state: &str, page: &str) -> Result<std::collections::HashMap<String, String>> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    let (mut stream, _) = loop {
        if Instant::now() >= deadline {
            bail!("no sign-in callback arrived in {} minutes; start the sign-in again", timeout.as_secs() / 60);
        }
        match listener.accept() {
            Ok(accepted) => break accepted,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e).context("waiting for the sign-in callback"),
        }
    };
    let query = callback_query(&mut stream, expected_state)?;
    let answer = format!("HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\nconnection: close\r\n\r\n<title>yeda</title><p>{page}</p>");
    let _ = stream.write_all(answer.as_bytes());
    let _ = stream.flush();
    Ok(query)
}

/// The token endpoint's answer, or its error body: the provider says in the body why it refused.
pub fn token_response(url: &str, status: u16, text: &str) -> Result<Value> {
    if !(200..300).contains(&status) {
        bail!("{url}: HTTP {status}: {}", text.chars().take(300).collect::<String>());
    }
    serde_json::from_str(text).with_context(|| format!("{url}: response is not JSON"))
}

/// Percent-encodes everything but the unreserved characters, as the docs ask for every value.
pub fn encode(v: &str) -> String {
    let mut out = String::new();
    for byte in v.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(*byte as char),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 3 <= bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pkce_verifier_has_the_length_the_spec_requires() {
        let (verifier, challenge) = pkce();
        let n = verifier.chars().count();
        assert!((43..=128).contains(&n), "{n}");
        assert!(verifier.chars().all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c)));
        assert_eq!(challenge.len(), 43);
    }

    #[test]
    fn percent_decodes_query_values() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn a_refused_exchange_reports_the_endpoints_own_reason() {
        let body = r#"{"error":"invalid_grant","error_description":"code expired"}"#;
        let err = token_response("https://auth.example/token", 400, body).unwrap_err().to_string();
        assert!(err.contains("HTTP 400") && err.contains("code expired"), "{err}");
        let ok = token_response("https://auth.example/token", 200, r#"{"access_token":"a"}"#).unwrap();
        assert_eq!(ok["access_token"], "a");
        assert!(token_response("https://auth.example/token", 200, "not json").is_err());
    }

    #[test]
    fn reads_the_callback_without_waiting_for_the_connection_to_close() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let query_of = |request: String| {
            let client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            std::thread::spawn(move || {
                let mut client = client;
                client.write_all(request.as_bytes()).unwrap();
                // A keep-alive connection: the browser does not close, so reading to EOF hangs.
                std::thread::sleep(Duration::from_secs(2));
                let _ = client.read_to_string(&mut String::new());
            });
            let (mut stream, _) = listener.accept().unwrap();
            callback_query(&mut stream, "s")
        };
        let ok = "GET /auth/callback?code=x&state=s&client_id=oaiapp_x HTTP/1.1\r\nhost: h\r\nconnection: keep-alive\r\n\r\n".to_string();
        let params = query_of(ok).unwrap();
        assert_eq!(params.get("code").map(String::as_str), Some("x"));
        assert_eq!(params.get("client_id").map(String::as_str), Some("oaiapp_x"));

        // A state that does not belong to this attempt is refused before the exchange.
        let bad = "GET /auth/callback?code=x&state=other HTTP/1.1\r\n\r\n".to_string();
        let mismatch = query_of(bad).unwrap_err();
        assert!(mismatch.to_string().contains("does not belong"), "{mismatch}");
    }
}
