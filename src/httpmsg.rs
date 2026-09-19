use std::io::{self, BufRead, Read, Write};

/// A deliberately minimal HTTP/1.1 message model: request line/status
/// line, headers, and a `Content-Length`-sized body. No chunked transfer
/// encoding — a real v1 limitation (see README), not an oversight; every
/// caller in this crate (`record.rs`/`replay.rs`) controls both ends of
/// each connection, so it's in a position to just not use chunked
/// encoding rather than needing to parse it.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn read_headers<R: BufRead>(reader: &mut R) -> io::Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Ok(headers)
}

/// Used for request bodies: absent `Content-Length` means no body. This
/// is a deliberate rule for *requests only* — a real client sending a
/// body with unspecified length isn't valid HTTP, and every request this
/// crate itself writes (`write_request`) always sets the header, so
/// nothing legitimate depends on a fallback here.
fn read_body<R: Read>(reader: &mut R, headers: &[(String, String)]) -> io::Result<Vec<u8>> {
    let len = header_value(headers, "content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body)?;
    }
    Ok(body)
}

/// Used for response bodies: a real upstream server omitting
/// `Content-Length` and instead closing the connection to signal "body's
/// over" is standard, legal HTTP/1.0 behavior (Python's stdlib
/// `http.server` does exactly this by default) — caught live when
/// `record` mode against a real Python server silently recorded every
/// response body as empty. Falls back to reading until EOF. Bounded by
/// the caller setting a read timeout on the socket (see `record.rs`) —
/// a pathological server that neither sends `Content-Length` nor closes
/// the connection would otherwise hang this forever.
fn read_body_or_until_eof<R: Read>(
    reader: &mut R,
    headers: &[(String, String)],
) -> io::Result<Vec<u8>> {
    match header_value(headers, "content-length").and_then(|v| v.parse::<usize>().ok()) {
        Some(len) => {
            let mut body = vec![0u8; len];
            if len > 0 {
                reader.read_exact(&mut body)?;
            }
            Ok(body)
        }
        None => {
            let mut body = Vec::new();
            reader.read_to_end(&mut body)?;
            Ok(body)
        }
    }
}

pub fn read_request<R: BufRead>(reader: &mut R) -> io::Result<Request> {
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let headers = read_headers(reader)?;
    let body = read_body(reader, &headers)?;
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

pub fn read_response<R: BufRead>(reader: &mut R) -> io::Result<Response> {
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    let headers = read_headers(reader)?;
    let body = read_body_or_until_eof(reader, &headers)?;
    Ok(Response {
        status,
        headers,
        body,
    })
}

/// Always emits a correct `Content-Length` for `req.body`, discarding
/// whatever (if anything) was in `req.headers` for it — same rule
/// `write_response` already follows, and for the same reason: an earlier
/// version of this function trusted the caller to have set
/// `Content-Length` itself, which silently dropped every request body
/// (the reader always defaults to a zero-length body when the header is
/// absent) until the round-trip test caught it.
pub fn write_request<W: Write>(writer: &mut W, req: &Request) -> io::Result<()> {
    write!(writer, "{} {} HTTP/1.1\r\n", req.method, req.path)?;
    for (k, v) in &req.headers {
        if k.eq_ignore_ascii_case("content-length") {
            continue;
        }
        write!(writer, "{k}: {v}\r\n")?;
    }
    write!(writer, "content-length: {}\r\n", req.body.len())?;
    write!(writer, "\r\n")?;
    writer.write_all(&req.body)?;
    writer.flush()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

/// Writes a response with headers taken from `resp.headers`, minus
/// `Content-Length`/`Transfer-Encoding`/`Connection` (this always sends
/// its own canonical, correct versions of those three, regardless of what
/// the original upstream response — or a stale recording — said).
pub fn write_response<W: Write>(writer: &mut W, resp: &Response) -> io::Result<()> {
    write!(
        writer,
        "HTTP/1.1 {} {}\r\n",
        resp.status,
        reason_phrase(resp.status)
    )?;
    for (k, v) in &resp.headers {
        if matches!(
            k.to_ascii_lowercase().as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        write!(writer, "{k}: {v}\r\n")?;
    }
    write!(writer, "content-length: {}\r\n", resp.body.len())?;
    write!(writer, "connection: close\r\n\r\n")?;
    writer.write_all(&resp.body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn round_trips_a_request_with_a_body() {
        let req = Request {
            method: "POST".to_string(),
            path: "/users".to_string(),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: b"{\"name\":\"ada\"}".to_vec(),
        };
        let mut buf = Vec::new();
        write_request(&mut buf, &req).unwrap();

        let mut reader = BufReader::new(&buf[..]);
        let parsed = read_request(&mut reader).unwrap();
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.path, "/users");
        assert_eq!(parsed.body, b"{\"name\":\"ada\"}");
        // write_request always sets a correct content-length itself
        // (see its doc comment for the bug this fixed) — this is the
        // actual mechanism that makes the body round-trip at all.
        assert_eq!(header_value(&parsed.headers, "content-length"), Some("14"));
    }

    #[test]
    fn write_request_ignores_a_caller_supplied_wrong_content_length() {
        let req = Request {
            method: "POST".to_string(),
            path: "/x".to_string(),
            headers: vec![("Content-Length".to_string(), "999".to_string())],
            body: b"short".to_vec(),
        };
        let mut buf = Vec::new();
        write_request(&mut buf, &req).unwrap();
        let mut reader = BufReader::new(&buf[..]);
        let parsed = read_request(&mut reader).unwrap();
        assert_eq!(parsed.body, b"short");
        assert_eq!(header_value(&parsed.headers, "content-length"), Some("5"));
    }

    #[test]
    fn request_with_no_body_reads_back_empty() {
        let req = Request {
            method: "GET".to_string(),
            path: "/health".to_string(),
            headers: vec![],
            body: vec![],
        };
        let mut buf = Vec::new();
        write_request(&mut buf, &req).unwrap();
        let mut reader = BufReader::new(&buf[..]);
        let parsed = read_request(&mut reader).unwrap();
        assert!(parsed.body.is_empty());
    }

    #[test]
    fn response_with_no_content_length_reads_body_until_eof() {
        // The exact bug caught live against a real Python http.server,
        // which omits Content-Length by default: raw bytes here, not
        // built via `write_response` (which would mask the bug by always
        // adding the header itself).
        let raw =
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nhello, no length header here";
        let mut reader = BufReader::new(&raw[..]);
        let parsed = read_response(&mut reader).unwrap();
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.body, b"hello, no length header here");
    }

    #[test]
    fn round_trips_a_response_and_always_sets_a_correct_content_length() {
        let resp = Response {
            status: 200,
            headers: vec![
                ("Content-Type".to_string(), "text/plain".to_string()),
                ("Content-Length".to_string(), "999".to_string()),
            ],
            body: b"hello world".to_vec(),
        };
        let mut buf = Vec::new();
        write_response(&mut buf, &resp).unwrap();

        let mut reader = BufReader::new(&buf[..]);
        let parsed = read_response(&mut reader).unwrap();
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.body, b"hello world");
        // The stale/wrong "999" from the input headers must not survive —
        // the real body length (11) is what actually gets sent and parsed back.
        assert_eq!(header_value(&parsed.headers, "content-length"), Some("11"));
    }

    #[test]
    fn unknown_status_code_still_produces_a_parseable_response() {
        let resp = Response {
            status: 599,
            headers: vec![],
            body: vec![],
        };
        let mut buf = Vec::new();
        write_response(&mut buf, &resp).unwrap();
        let mut reader = BufReader::new(&buf[..]);
        assert_eq!(read_response(&mut reader).unwrap().status, 599);
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let headers = vec![("Content-Type".to_string(), "text/plain".to_string())];
        assert_eq!(header_value(&headers, "content-type"), Some("text/plain"));
        assert_eq!(header_value(&headers, "CONTENT-TYPE"), Some("text/plain"));
    }
}
