use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use mote::repo::Store;
use mote::server::{ServerContext, ServerSecurity};
use tempfile::tempdir;

const TOKEN: &str = "00112233445566778899aabbccddeeff";

#[test]
fn public_console_entrypoints_refuse_remote_stores_before_local_effects() {
    let temp = tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    fs::write(store.root().join("remote.json"), b"{}\n").unwrap();

    assert!(ServerContext::new(store.clone(), ServerSecurity::new(0, TOKEN)).is_err());
    assert!(mote::tui::run(&store, None).is_err());

    // Keeping the requested port occupied proves the authority fence runs
    // before the listener bind. No launch token may be written either.
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let error =
        mote::server::serve(store.clone(), occupied.local_addr().unwrap().port()).unwrap_err();
    assert!(error.to_string().contains("Git-authority"), "{error}");
    assert!(!store.local_dir().join("serve-token").exists());
}

#[test]
fn existing_server_context_stops_before_serving_a_cached_snapshot_after_fencing() {
    let temp = tempdir().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let context =
        ServerContext::new(store.clone(), ServerSecurity::new(address.port(), TOKEN)).unwrap();

    // This mirrors a mode transition after the console has already created its
    // watcher and in-memory projection.
    fs::write(store.root().join("remote.json"), b"{}\n").unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        mote::server::serve_connection(stream, &context)
    });

    let mut client = TcpStream::connect(address).unwrap();
    client
        .write_all(
            format!(
                "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Mote-Token: {TOKEN}\r\n\r\n",
                address.port()
            )
            .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    match client.read_to_string(&mut response) {
        Ok(_) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ) => {}
        Err(error) => panic!("unexpected client read failure: {error}"),
    }

    let error = server.join().unwrap().unwrap_err();
    assert!(error.to_string().contains("Git-authority"), "{error}");
    assert!(response.is_empty(), "fenced context served: {response}");
}
