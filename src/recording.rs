use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedExchange {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub request_body: Vec<u8>,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub response_body: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    pub entries: Vec<RecordedExchange>,
}

/// Finds the recorded exchange to replay for an incoming `(method, path,
/// body)`. Tries an exact match on all three first — this is what
/// distinguishes two `POST /users` calls with different bodies — and
/// falls back to method+path only, so idempotent `GET`s (and the common
/// case of a body that doesn't matter for matching) still replay even
/// though their bodies are usually empty and identical anyway.
pub fn find_match<'a>(
    recording: &'a Recording,
    method: &str,
    path: &str,
    body: &[u8],
) -> Option<&'a RecordedExchange> {
    recording
        .entries
        .iter()
        .find(|e| e.method == method && e.path == path && e.request_body == body)
        .or_else(|| {
            recording
                .entries
                .iter()
                .find(|e| e.method == method && e.path == path)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(method: &str, path: &str, body: &[u8], response_body: &str) -> RecordedExchange {
        RecordedExchange {
            method: method.to_string(),
            path: path.to_string(),
            request_body: body.to_vec(),
            status: 200,
            headers: vec![],
            response_body: response_body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn matches_by_method_and_path_when_bodies_are_empty() {
        let recording = Recording {
            entries: vec![exchange("GET", "/users", b"", "user list")],
        };
        let found = find_match(&recording, "GET", "/users", b"").unwrap();
        assert_eq!(found.response_body, b"user list");
    }

    #[test]
    fn distinguishes_two_requests_to_the_same_path_by_body() {
        let recording = Recording {
            entries: vec![
                exchange("POST", "/users", br#"{"name":"ada"}"#, "created ada"),
                exchange("POST", "/users", br#"{"name":"grace"}"#, "created grace"),
            ],
        };
        let ada = find_match(&recording, "POST", "/users", br#"{"name":"ada"}"#).unwrap();
        assert_eq!(ada.response_body, b"created ada");
        let grace = find_match(&recording, "POST", "/users", br#"{"name":"grace"}"#).unwrap();
        assert_eq!(grace.response_body, b"created grace");
    }

    #[test]
    fn falls_back_to_method_and_path_when_the_exact_body_wasnt_recorded() {
        let recording = Recording {
            entries: vec![exchange(
                "POST",
                "/users",
                br#"{"name":"ada"}"#,
                "created ada",
            )],
        };
        // A different body than what was recorded — still matches on
        // method+path as the fallback, rather than returning nothing.
        let found = find_match(&recording, "POST", "/users", br#"{"name":"someone else"}"#);
        assert!(found.is_some());
    }

    #[test]
    fn no_match_for_an_unrecorded_path() {
        let recording = Recording {
            entries: vec![exchange("GET", "/users", b"", "user list")],
        };
        assert!(find_match(&recording, "GET", "/orders", b"").is_none());
    }

    #[test]
    fn recording_round_trips_through_json() {
        let recording = Recording {
            entries: vec![exchange("GET", "/health", b"", "ok")],
        };
        let json = serde_json::to_string(&recording).unwrap();
        let parsed: Recording = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, recording);
    }
}
