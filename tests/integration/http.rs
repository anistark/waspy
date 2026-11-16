//! Outgoing HTTP under WASI preview 2: a program using `requests` (or the
//! synchronous Anthropic client) is compiled to a component and run with
//! `wasmtime run -S http` against a server in the test, on 127.0.0.1.
//!
//! The server answers as `server.py` below would (the same bodies, keys in
//! the same order), and the expected output is what CPython 3.13 with the
//! real `requests` 2.34 prints for the same program against it:
//!
//! ```python
//! # server.py: GET echoes the path, User-Agent, and X-Token as JSON
//! # (/missing is a 404 in text), POST and PUT echo the body, its
//! # Content-Type, and its length, and POST /v1/messages answers as the
//! # Messages API does for the key "test-key" (a 401 for any other).
//! ```
//!
//! wasmtime is required where `WASPY_REQUIRE_WASMTIME` is set (CI's runtime
//! job); elsewhere a missing wasmtime skips the test with a message.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;

fn quoted(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

/// One request read and answered.
fn serve(stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).unwrap();
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_lowercase(), v.trim().to_string()));
        }
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let length: usize = header("content-length")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).unwrap();
    let body = String::from_utf8(body).unwrap();
    let json_or_null = |v: Option<String>| v.map(|s| quoted(&s)).unwrap_or("null".into());
    if path.starts_with("/old") {
        let mut out = stream;
        let _ = write!(
            out,
            "HTTP/1.1 302 Found\r\nLocation: /items?moved=1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        return;
    }
    let (status, reason, kind, text) = if method == "GET" && path.starts_with("/missing") {
        (404, "Not Found", "text/plain", "nothing here".to_string())
    } else if method == "GET" {
        let text = format!(
            "{{\"path\": {}, \"agent\": {}, \"token\": {}}}",
            quoted(&path),
            json_or_null(header("user-agent")),
            json_or_null(header("x-token"))
        );
        (200, "OK", "application/json", text)
    } else if path == "/v1/messages" {
        if header("x-api-key").as_deref() != Some("test-key") {
            let text = r#"{"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}}"#;
            (401, "Unauthorized", "application/json", text.to_string())
        } else {
            let sent: serde_json::Value = serde_json::from_str(&body).unwrap();
            let last = sent["messages"][0]["content"].as_str().unwrap_or("");
            let text = format!(
                "{{\"id\": \"msg_1\", \"type\": \"message\", \"role\": \"assistant\", \"model\": {}, \"content\": [{{\"type\": \"text\", \"text\": {}}}], \"stop_reason\": \"end_turn\", \"usage\": {{\"input_tokens\": 3, \"output_tokens\": 5}}}}",
                sent["model"],
                quoted(&format!("heard: {last}"))
            );
            (200, "OK", "application/json", text)
        }
    } else {
        let text = format!(
            "{{\"got\": {}, \"type\": {}, \"length\": {length}}}",
            quoted(&body),
            json_or_null(header("content-type"))
        );
        (200, "OK", "application/json", text)
    };
    let mut out = stream;
    let _ = write!(
        out,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nX-Echo-Method: {method}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
}

/// A server on a free port, answering until the test ends.
fn server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || serve(stream));
        }
    });
    port
}

/// Compile `source` (with `{PORT}` replaced) and run it under wasmtime with
/// HTTP; its stdout, or `None` when wasmtime is not there.
fn run(name: &str, source: &str, port: u16) -> Option<String> {
    let source = source.replace("{PORT}", &port.to_string());
    let wasm = waspy::compile_python_to_wasm(&source).expect("the program compiles");
    assert_eq!(&wasm[4..8], &[0x0d, 0x00, 0x01, 0x00], "a component");
    let path = std::env::temp_dir().join(format!("waspy-http-{}-{name}.wasm", std::process::id()));
    std::fs::write(&path, &wasm).unwrap();
    let out = Command::new("wasmtime")
        .args(["run", "-S", "http"])
        .arg(&path)
        .output();
    let _ = std::fs::remove_file(&path);
    match out {
        Ok(out) => {
            assert_eq!(String::from_utf8_lossy(&out.stderr), "", "stderr");
            Some(String::from_utf8_lossy(&out.stdout).to_string())
        }
        Err(_) if std::env::var_os("WASPY_REQUIRE_WASMTIME").is_none() => {
            eprintln!("wasmtime not found; skipped");
            None
        }
        Err(e) => panic!("wasmtime is required here: {e}"),
    }
}

/// requests: a GET with query parameters and a header, a JSON body, a form
/// body, a body larger than one write, a 404 and `raise_for_status()`, and
/// a URL with no scheme, as requests 2.34 answers each.
#[test]
fn requests_over_wasi_http() {
    let port = server();
    let source = r#"
import requests

BASE = "http://127.0.0.1:{PORT}"


def main() -> None:
    r = requests.get(BASE + "/items", params={"q": "a b", "n": 2}, headers={"X-Token": "t1"})
    print(r.status_code, r.reason, r.ok, r.headers["Content-Type"], r.headers.get("x-echo-method"))
    print(r.json())
    r = requests.post(BASE + "/submit", json={"name": "pen", "tags": ["x"]})
    print(r.json())
    r = requests.post(BASE + "/form", data={"a": "1 2", "b": "x"})
    print(r.json())
    big = "z" * 10000
    r = requests.put(BASE + "/big", data=big)
    print(r.json()["length"])
    r = requests.get(BASE + "/missing")
    print(r.status_code, r.text, r)
    try:
        r.raise_for_status()
    except requests.HTTPError as e:
        print(type(e).__name__, e)
    try:
        requests.get("127.0.0.1/x")
    except requests.RequestException as e:
        print(type(e).__name__, e)
    r = requests.post(BASE + "/old", json={"a": 1})
    print(r.status_code, r.json()["path"], [h.status_code for h in r.history], r.url)
    r = requests.get(BASE + "/old", allow_redirects=False)
    print(r.status_code, r.is_redirect, r.headers["Location"])


if __name__ == "__main__":
    main()
"#;
    let Some(out) = run("requests", source, port) else {
        return;
    };
    let want = "200 OK True application/json GET
{'path': '/items?q=a+b&n=2', 'agent': 'python-requests/2.34.2', 'token': 't1'}
{'got': '{\"name\": \"pen\", \"tags\": [\"x\"]}', 'type': 'application/json', 'length': 30}
{'got': 'a=1+2&b=x', 'type': 'application/x-www-form-urlencoded', 'length': 9}
10000
404 nothing here <Response [404]>
HTTPError 404 Client Error: Not Found for url: http://127.0.0.1:{PORT}/missing
MissingSchema Invalid URL '127.0.0.1/x': No scheme supplied. Perhaps you meant https://127.0.0.1/x?
200 /items?moved=1 [302] http://127.0.0.1:{PORT}/items?moved=1
302 True /items?moved=1
"
    .replace("{PORT}", &port.to_string());
    assert_eq!(out, want);
}

/// The synchronous Anthropic client over requests: a message read back, a
/// 401 raising AuthenticationError, and a refused connection raising
/// requests' ConnectionError and the client's APIConnectionError.
#[test]
fn anthropic_client_over_wasi_http() {
    let port = server();
    // A port nothing listens on.
    let closed = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let source = r#"
import anthropic
import requests


def main() -> None:
    client = anthropic.Anthropic(api_key="test-key", base_url="http://127.0.0.1:{PORT}")
    message = client.messages.create(
        model="claude-opus-5-5",
        max_tokens=64,
        messages=[{"role": "user", "content": "ping"}],
    )
    print(message.content[0].text, message.stop_reason, message.usage.output_tokens, message.model)
    try:
        anthropic.Anthropic(api_key="wrong", base_url="http://127.0.0.1:{PORT}").messages.create(model="m", max_tokens=1, messages=[{"role": "user", "content": "x"}])
    except anthropic.AuthenticationError as e:
        print(type(e).__name__, e.status_code, e.type)
    try:
        requests.get("http://127.0.0.1:CLOSED/")
    except requests.ConnectionError as e:
        print(type(e).__name__, str(e).split(" (Caused")[0])
    try:
        anthropic.Anthropic(api_key="k", base_url="http://127.0.0.1:CLOSED").messages.create(model="m", max_tokens=1, messages=[{"role": "user", "content": "x"}])
    except anthropic.APIConnectionError as e:
        print(type(e).__name__, e)


if __name__ == "__main__":
    main()
"#
    .replace("CLOSED", &closed.to_string());
    let Some(out) = run("anthropic", &source, port) else {
        return;
    };
    let want = format!(
        "heard: ping end_turn 5 claude-opus-5-5\n\
         AuthenticationError 401 authentication_error\n\
         ConnectionError HTTPConnectionPool(host='127.0.0.1', port={closed}): Max retries exceeded with url: /\n\
         APIConnectionError Connection error.\n"
    );
    assert_eq!(out, want);
}
