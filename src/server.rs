//! Small blocking HTTP/1.1 core for the loopback console server.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::errors::MoteResult;
use crate::repo::Store;

const MAX_REQUEST_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 32 * 1024;
const MAX_BODY: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 64;

pub fn serve(store: Store, bind: &str) -> MoteResult<()> {
    let listener = TcpListener::bind(bind)?;
    let active = Arc::new(AtomicUsize::new(0));
    eprintln!("mote console listening on http://{bind}");
    for incoming in listener.incoming() {
        let mut stream = incoming?;
        let previous = active.fetch_add(1, Ordering::AcqRel);
        if previous >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            write_response(
                &mut stream,
                503,
                br#"{"message":"connection limit reached"}"#,
            )?;
            continue;
        }
        let active = Arc::clone(&active);
        let store = store.clone();
        std::thread::spawn(move || {
            let _ = serve_connection(stream, &store);
            active.fetch_sub(1, Ordering::AcqRel);
        });
    }
    Ok(())
}

pub fn serve_connection(mut stream: TcpStream, store: &Store) -> MoteResult<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            let body = serde_json::to_vec(&serde_json::json!({"message": error.message}))?;
            return write_response(&mut stream, error.status, &body);
        }
    };
    if request.path == "/api/health" && request.method == "GET" {
        let format = store.read_format()?;
        let body = serde_json::to_vec(&serde_json::json!({
            "ok": true,
            "store_id": format.store_id,
        }))?;
        return write_response(&mut stream, 200, &body);
    }
    write_response(&mut stream, 404, br#"{"message":"route not found"}"#)
}

struct Request {
    method: String,
    path: String,
    #[allow(dead_code)]
    body: Vec<u8>,
}

struct HttpError {
    status: u16,
    message: String,
}

fn read_request(stream: &mut TcpStream) -> Result<Request, HttpError> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if bytes.len() >= MAX_HEADERS {
            return Err(http_error(431, "header block exceeds 32 KiB"));
        }
        let mut buffer = [0_u8; 4096];
        let count = stream
            .read(&mut buffer)
            .map_err(|error| http_error(400, error.to_string()))?;
        if count == 0 {
            return Err(http_error(400, "incomplete HTTP headers"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    };
    if header_end > MAX_HEADERS {
        return Err(http_error(431, "header block exceeds 32 KiB"));
    }
    let header = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| http_error(400, "headers are not UTF-8"))?;
    let mut lines = header.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    if request_line.len() > MAX_REQUEST_LINE {
        return Err(http_error(414, "request line exceeds 8 KiB"));
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or_default();
    if parts.next().is_some() || !version.starts_with("HTTP/1.") {
        return Err(http_error(400, "malformed request line"));
    }
    if !matches!(method.as_str(), "GET" | "POST" | "PATCH" | "DELETE") {
        return Err(http_error(405, "method not allowed"));
    }
    let mut content_length = 0_usize;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| http_error(400, "malformed header"))?;
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value.trim().eq_ignore_ascii_case("chunked")
        {
            return Err(http_error(400, "chunked request bodies are unsupported"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value
                .trim()
                .parse()
                .map_err(|_| http_error(400, "invalid Content-Length"))?;
        }
    }
    if content_length > MAX_BODY {
        return Err(http_error(413, "request body exceeds 1 MiB"));
    }
    while bytes.len() < header_end + content_length {
        let mut buffer = [0_u8; 8192];
        let count = stream
            .read(&mut buffer)
            .map_err(|error| http_error(400, error.to_string()))?;
        if count == 0 {
            return Err(http_error(400, "incomplete request body"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let raw_path = target.split('?').next().unwrap_or_default();
    let path = percent_decode(raw_path)?;
    if path.split('/').any(|segment| segment == "..") {
        return Err(http_error(400, "parent path segments are forbidden"));
    }
    Ok(Request {
        method,
        path,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

fn percent_decode(path: &str) -> Result<String, HttpError> {
    let input = path.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' {
            if index + 2 >= input.len() {
                return Err(http_error(400, "invalid percent-encoding"));
            }
            let digits = std::str::from_utf8(&input[index + 1..index + 3])
                .map_err(|_| http_error(400, "invalid percent-encoding"))?;
            output.push(
                u8::from_str_radix(digits, 16)
                    .map_err(|_| http_error(400, "invalid percent-encoding"))?,
            );
            index += 3;
        } else {
            output.push(input[index]);
            index += 1;
        }
    }
    String::from_utf8(output).map_err(|_| http_error(400, "path is not UTF-8"))
}

fn http_error(status: u16, message: impl Into<String>) -> HttpError {
    HttpError {
        status,
        message: message.into(),
    }
}

fn write_response(stream: &mut TcpStream, status: u16, body: &[u8]) -> MoteResult<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}
