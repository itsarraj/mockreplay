use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};

use crate::httpmsg::{self, Response};
use crate::recording::{self, Recording};

/// Serves `recording` on an already-bound `listener` (see `record::serve`
/// for why binding is the caller's job). Every incoming request is
/// matched against the recording and answered from it — no network calls
/// out, deterministic, offline. An unmatched request gets a `404` naming
/// the method+path that wasn't found, not a silent empty response.
pub fn serve(listener: TcpListener, recording: Recording) -> Result<()> {
    let recording = Arc::new(recording);
    for stream in listener.incoming() {
        let stream = stream?;
        let recording = Arc::clone(&recording);
        thread::spawn(move || {
            let _ = handle_one(stream, &recording);
        });
    }
    Ok(())
}

fn handle_one(client: TcpStream, recording: &Recording) -> Result<()> {
    let mut reader = BufReader::new(client.try_clone().context("cloning client stream")?);
    let req = httpmsg::read_request(&mut reader).context("reading client request")?;

    let resp = match recording::find_match(recording, &req.method, &req.path, &req.body) {
        Some(entry) => Response {
            status: entry.status,
            headers: entry.headers.clone(),
            body: entry.response_body.clone(),
        },
        None => Response {
            status: 404,
            headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
            body: format!(
                "mockreplay: no recorded exchange for {} {}",
                req.method, req.path
            )
            .into_bytes(),
        },
    };

    let mut writer = client;
    httpmsg::write_response(&mut writer, &resp).context("writing replay response")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::httpmsg::Request;
    use crate::recording::RecordedExchange;
    use std::io::Write;

    fn start_replay_server(recording: Recording) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            serve(listener, recording).ok();
        });
        thread::sleep(std::time::Duration::from_millis(100));
        addr
    }

    fn send_request(addr: std::net::SocketAddr, method: &str, path: &str, body: &[u8]) -> Response {
        let mut client = TcpStream::connect(addr).unwrap();
        let req = Request {
            method: method.to_string(),
            path: path.to_string(),
            headers: vec![],
            body: body.to_vec(),
        };
        httpmsg::write_request(&mut client, &req).unwrap();
        client.flush().unwrap();
        let mut reader = BufReader::new(client);
        httpmsg::read_response(&mut reader).unwrap()
    }

    #[test]
    fn replays_a_recorded_response_exactly() {
        let recording = Recording {
            entries: vec![RecordedExchange {
                method: "GET".to_string(),
                path: "/health".to_string(),
                request_body: vec![],
                status: 200,
                headers: vec![("Content-Type".to_string(), "application/json".to_string())],
                response_body: b"{\"status\":\"ok\"}".to_vec(),
            }],
        };
        let addr = start_replay_server(recording);

        let resp = send_request(addr, "GET", "/health", b"");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"{\"status\":\"ok\"}");
    }

    #[test]
    fn unrecorded_request_gets_a_404_naming_what_was_missing() {
        let addr = start_replay_server(Recording::default());
        let resp = send_request(addr, "GET", "/nonexistent", b"");
        assert_eq!(resp.status, 404);
        assert!(String::from_utf8_lossy(&resp.body).contains("/nonexistent"));
    }

    #[test]
    fn distinguishes_requests_by_body_when_replaying() {
        let recording = Recording {
            entries: vec![
                RecordedExchange {
                    method: "POST".to_string(),
                    path: "/echo".to_string(),
                    request_body: b"a".to_vec(),
                    status: 200,
                    headers: vec![],
                    response_body: b"got a".to_vec(),
                },
                RecordedExchange {
                    method: "POST".to_string(),
                    path: "/echo".to_string(),
                    request_body: b"b".to_vec(),
                    status: 200,
                    headers: vec![],
                    response_body: b"got b".to_vec(),
                },
            ],
        };
        let addr = start_replay_server(recording);

        assert_eq!(send_request(addr, "POST", "/echo", b"a").body, b"got a");
        assert_eq!(send_request(addr, "POST", "/echo", b"b").body, b"got b");
    }
}
