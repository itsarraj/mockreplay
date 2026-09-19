use std::fs;
use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};

use crate::httpmsg;
use crate::recording::{RecordedExchange, Recording};

pub fn load_or_default(path: &Path) -> Recording {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Runs the record proxy on an already-bound `listener` (bound separately
/// so tests can use an OS-assigned port and read it back before this
/// takes ownership — see `serve` for why). Blocks forever, one thread per
/// connection. Every real exchange is forwarded to `target_addr` (plain
/// HTTP only — see README), returned to the caller unchanged, and
/// appended to `out_path` immediately, so a killed process doesn't lose
/// what was already recorded.
pub fn serve(listener: TcpListener, target_addr: String, out_path: PathBuf) -> Result<()> {
    let recording = Arc::new(Mutex::new(load_or_default(&out_path)));
    for stream in listener.incoming() {
        let stream = stream?;
        let recording = Arc::clone(&recording);
        let target_addr = target_addr.clone();
        let out_path = out_path.clone();
        thread::spawn(move || {
            let _ = handle_one(stream, &target_addr, &recording, &out_path);
        });
    }
    Ok(())
}

fn handle_one(
    client: TcpStream,
    target_addr: &str,
    recording: &Mutex<Recording>,
    out_path: &Path,
) -> Result<()> {
    let mut reader = BufReader::new(client.try_clone().context("cloning client stream")?);
    let req = httpmsg::read_request(&mut reader).context("reading client request")?;

    let mut upstream = TcpStream::connect(target_addr)
        .with_context(|| format!("connecting to upstream {target_addr}"))?;
    // A response with no Content-Length is read until the upstream closes
    // the connection (see `read_body_or_until_eof`) — this timeout bounds
    // how long that can take if an upstream does neither.
    upstream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .ok();
    httpmsg::write_request(&mut upstream, &req).context("forwarding request upstream")?;
    let mut upstream_reader = BufReader::new(upstream);
    let resp = httpmsg::read_response(&mut upstream_reader).context("reading upstream response")?;

    let mut client_writer = client;
    httpmsg::write_response(&mut client_writer, &resp).context("returning response to client")?;

    let entry = RecordedExchange {
        method: req.method,
        path: req.path,
        request_body: req.body,
        status: resp.status,
        headers: resp.headers,
        response_body: resp.body,
    };
    let mut guard = recording.lock().expect("recording mutex poisoned");
    guard.entries.push(entry);
    let json = serde_json::to_string_pretty(&*guard).context("serializing recording")?;
    fs::write(out_path, json).with_context(|| format!("writing {}", out_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::httpmsg::{Request, Response};
    use std::io::Write;

    /// A tiny fake "upstream" that answers every request with one fixed
    /// response — enough to prove the record proxy forwards correctly
    /// without depending on any real external API being reachable.
    fn spawn_fake_upstream(response: Response) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let _req = httpmsg::read_request(&mut reader).unwrap();
                let mut writer = stream;
                httpmsg::write_response(&mut writer, &response).unwrap();
            }
        });
        addr
    }

    #[test]
    fn forwards_to_upstream_and_records_the_real_exchange() {
        let upstream_addr = spawn_fake_upstream(Response {
            status: 201,
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: b"{\"id\":42}".to_vec(),
        });

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let record_addr = listener.local_addr().unwrap();
        let out_path = std::env::temp_dir().join(format!(
            "mockreplay-test-record-{}.json",
            std::process::id()
        ));
        let out_path_clone = out_path.clone();
        thread::spawn(move || {
            serve(listener, upstream_addr.to_string(), out_path_clone).ok();
        });
        thread::sleep(std::time::Duration::from_millis(100));

        let mut client = TcpStream::connect(record_addr).unwrap();
        let req = Request {
            method: "POST".to_string(),
            path: "/users".to_string(),
            headers: vec![("Host".to_string(), "example.test".to_string())],
            body: b"{\"name\":\"ada\"}".to_vec(),
        };
        httpmsg::write_request(&mut client, &req).unwrap();
        client.flush().unwrap();
        let mut reader = BufReader::new(client);
        let resp = httpmsg::read_response(&mut reader).unwrap();

        assert_eq!(resp.status, 201);
        assert_eq!(resp.body, b"{\"id\":42}");

        thread::sleep(std::time::Duration::from_millis(100));
        let saved: Recording =
            serde_json::from_str(&fs::read_to_string(&out_path).unwrap()).unwrap();
        assert_eq!(saved.entries.len(), 1);
        assert_eq!(saved.entries[0].method, "POST");
        assert_eq!(saved.entries[0].path, "/users");
        assert_eq!(saved.entries[0].status, 201);
        assert_eq!(saved.entries[0].response_body, b"{\"id\":42}");

        fs::remove_file(&out_path).ok();
    }
}
