use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use tempfile::TempDir;

use mote::repo::Store;

fn exchange(request: Vec<u8>) -> String {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        mote::server::serve_connection(stream, &store).unwrap();
    });
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(&request).unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    server.join().unwrap();
    response
}

#[test]
fn health_is_json_and_every_response_closes_the_connection() {
    let response = exchange(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".to_vec());
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    assert!(response.contains("Connection: close\r\n"));
    assert!(response.contains("\"store_id\":\"st-"));
}

#[test]
fn parser_rejects_chunking_traversal_bad_methods_and_oversize_bodies() {
    let cases = [
        (
            b"POST /api/health HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(),
            "400 Bad Request",
        ),
        (
            b"GET /api/%2e%2e/secret HTTP/1.1\r\n\r\n".to_vec(),
            "400 Bad Request",
        ),
        (
            b"PUT /api/health HTTP/1.1\r\n\r\n".to_vec(),
            "405 Method Not Allowed",
        ),
        (
            b"POST /api/nope HTTP/1.1\r\nContent-Length: 1048577\r\n\r\n".to_vec(),
            "413 Payload Too Large",
        ),
    ];
    for (request, expected) in cases {
        let response = exchange(request);
        assert!(
            response.contains(expected),
            "expected {expected}: {response}"
        );
    }
}

#[test]
fn unsupported_routes_are_honest_404s() {
    let response = exchange(b"GET /api/not-yet-implemented HTTP/1.1\r\n\r\n".to_vec());
    assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
    assert!(response.contains("route not found"));
}
