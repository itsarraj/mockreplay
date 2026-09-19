# mockreplay

Record real HTTP traffic once, replay it for tests — a lighter
`WireMock` (Java) / `Mockoon` (Electron/Node) alternative. Point it at a
real API once, it records the exchanges; point your tests at the
recording instead of the network, so they stop being flaky because a
third-party API had a bad day.

## Usage

```bash
mockreplay record --target localhost:9000 --listen 127.0.0.1:8081 --out recording.json
# ... exercise your app against 127.0.0.1:8081 as if it were the real API ...

mockreplay replay --file recording.json --listen 127.0.0.1:8081
# same requests now served from the recording, zero network calls out
```

## Scope: plain HTTP targets only in v1

`record` forwards to a plain-HTTP upstream — no TLS. This is a
deliberate v1 cut, not an oversight: `replay` (arguably the more valuable
half — deterministic, offline test mocking) works regardless of what
protocol the *original* API used, since replaying only ever serves the
local HTTP server, no outbound calls at all. HTTPS-target support in
`record` is a real v2, not attempted here to keep this buildable in one
pass. Also no chunked transfer encoding (`Content-Length`-framed bodies
only, or a connection-close-terminated body — see below).

## Status: built, then genuinely caught and fixed two real bugs via live testing against a real server

- **16 unit tests** (`cargo test --lib`): HTTP message round-tripping
  (request with a body, request with none, a stale/wrong `Content-Length`
  in the input being ignored in favor of the real one, unknown status
  codes, case-insensitive header lookup); the recording matcher (method
  and path alone is enough to match a `GET`, two `POST`s to the same path
  distinguished by body, a fallback to method+path when the exact body
  wasn't recorded, no match returning `None` rather than a wrong guess);
  and a real record→replay round trip through an in-process fake upstream
  and a real `TcpListener` on an OS-assigned port (not a mock — a genuine
  socket, a genuine second thread acting as "the real API").
- **Bug #1, caught by the round-trip test itself**: the very first version
  of `write_request` never set `Content-Length`, so every request body —
  in both `record`'s client→proxy leg and `replay`'s test-client→server
  leg — silently arrived as empty on the receiving end. `write_response`
  already did this correctly; `write_request` didn't match it. Fixed to
  mirror the same rule, re-verified, and a `distinguishes_requests_by_body`
  test that had been silently passing for the wrong reason (both requests
  landing on the same recorded entry, since the empty bodies were
  indistinguishable) now correctly exercises what it claims to.
- **Bug #2, caught only by testing against a real external server, not
  by any unit test**: pointed `record` at a real Python `http.server`
  instance. Every response came back with `response_body: []` in the
  recording despite the upstream demonstrably sending real content
  (confirmed independently with `curl -v`, which showed `HTTP 1.0, assume
  close after body` — Python's stdlib server sends no `Content-Length` at
  all by default, framing the body by simply closing the connection, which
  is completely standard, legal HTTP/1.0 behavior). This crate's response
  reader had no fallback for that framing — it only knew `Content-Length`,
  defaulting silently to an empty body when the header was absent. Fixed
  with a `read_body_or_until_eof` path used specifically for responses (not
  requests, where an unspecified length isn't valid HTTP to begin with),
  bounded by a 30-second read timeout on the upstream socket so a
  pathological server that neither sends a length nor closes the
  connection can't hang the proxy forever. **Re-verified against the same
  real Python server**: `GET /health` and two distinct `POST /users`
  bodies all recorded correctly, then — with the Python server killed —
  `replay` served byte-identical responses from the recording alone, and
  correctly `404`'d a path that was never recorded.

**Not done / deliberately deferred**: HTTPS targets in `record` (see
Scope), chunked transfer-encoding support, and a `--delay` flag to
simulate the original API's real latency during replay (currently
instantaneous, which is usually what you want for tests but not always
for load-adjacent testing).
