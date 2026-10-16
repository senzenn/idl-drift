//! One JSON-RPC request over HTTP, with the retry rules that `idl-drift` and `idl-verify`
//! share.

use std::time::Duration;

use serde_json::{json, Value};

/// Tries per request.
const ATTEMPTS: u32 = 5;
/// Triton asks a client to stop for 10 s after a 429; an earlier retry gets another 429.
const RATE_LIMIT_WAIT: Duration = Duration::from_secs(10);
/// The longest `Retry-After` to obey, so that one header cannot stop a run for long.
const MAX_RETRY_AFTER: Duration = Duration::from_mins(1);
/// The largest answer to read: two accounts of the 10 MiB maximum, in base64.
const MAX_RESPONSE_BYTES: u64 = 32 << 20;
/// The largest error body to read for its message.
const MAX_ERROR_BYTES: u64 = 64 << 10;

/// An HTTP agent for [`call`]: a 30 s limit per request, and HTTP errors as responses.
#[must_use]
pub fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build(),
    )
}

/// Send one JSON-RPC request and return its `result`. A 429, a 5xx, a lost connection and
/// a cut-off answer are tried again, up to 5 tries in total.
///
/// # Errors
///
/// `<method>: <detail>`. The text never holds `url`, which can hold an API key.
pub fn call(agent: &ureq::Agent, url: &str, method: &str, params: &Value) -> Result<Value, String> {
    let fail = |detail: String| format!("{method}: {detail}");
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let mut last = String::new();
    for attempt in 0..ATTEMPTS {
        let wait = match agent.post(url).send_json(&body) {
            Err(e) if retryable(&e) => {
                last = transport_text(&e);
                backoff(attempt)
            }
            Err(e) => return Err(fail(transport_text(&e))),
            Ok(mut response) => match response.status().as_u16() {
                429 => {
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    last = format!("HTTP 429{}", error_message(&mut response));
                    rate_limit_wait(retry_after.as_deref())
                }
                status @ 500.. => {
                    last = format!("HTTP {status}{}", error_message(&mut response));
                    backoff(attempt)
                }
                200 => {
                    let read = response
                        .body_mut()
                        .with_config()
                        .limit(MAX_RESPONSE_BYTES)
                        .read_json::<Value>();
                    match read {
                        Ok(mut reply) => {
                            // `Value` prints strings JSON-escaped, so control characters stay inert.
                            if let Some(error) = reply.get("error") {
                                return Err(fail(error.to_string()));
                            }
                            return Ok(reply["result"].take());
                        }
                        Err(e) if retryable(&e) => {
                            last = transport_text(&e);
                            backoff(attempt)
                        }
                        Err(e) => return Err(fail(transport_text(&e))),
                    }
                }
                status => return Err(fail(format!("HTTP {status}"))),
            },
        };
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(wait);
        }
    }
    Err(fail(format!("{last}, after {ATTEMPTS} tries")))
}

/// A lost connection or a cut-off answer can succeed on the next try. A wrong host, a port
/// where nothing listens, or an answer over the size limit cannot.
fn retryable(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Io(io) => io.kind() != std::io::ErrorKind::ConnectionRefused,
        ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed | ureq::Error::Json(_) => true,
        _ => false,
    }
}

/// Other ureq errors can repeat the URL.
fn transport_text(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::BodyExceedsLimit(_)
        | ureq::Error::Json(_) => e.to_string(),
        _ => "the request failed; check that the RPC URL is a valid http(s) URL".into(),
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500 << attempt)
}

/// `Retry-After` in seconds when the RPC sends it, else the 10 s that Triton asks for.
fn rate_limit_wait(retry_after: Option<&str>) -> Duration {
    retry_after
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map_or(RATE_LIMIT_WAIT, |s| {
            Duration::from_secs(s).min(MAX_RETRY_AFTER)
        })
}

/// `: <message>` from a JSON-RPC error body, such as Helius's "credits exhausted", or nothing.
fn error_message(response: &mut ureq::http::Response<ureq::Body>) -> String {
    response
        .body_mut()
        .with_config()
        .limit(MAX_ERROR_BYTES)
        .read_json::<Value>()
        .ok()
        .and_then(|v| v.pointer("/error/message").cloned())
        .map_or_else(String::new, |m| format!(": {m}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// A local HTTP server that answers each request with the next of `replies`, each
    /// `(status line and headers, body)`, and closes the connection.
    fn serve(replies: Vec<(&'static str, &'static str)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for (head, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                reader.read_exact(&mut vec![0; length]).unwrap();
                let reply = format!(
                    "{head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        url
    }

    #[test]
    fn retry_after_and_a_cut_off_answer_are_tried_again() {
        let url = serve(vec![
            ("HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0", ""),
            ("HTTP/1.1 200 OK", r#"{"jsonrpc":"2.0","res"#),
            ("HTTP/1.1 200 OK", r#"{"jsonrpc":"2.0","id":1,"result":7}"#),
        ]);
        assert_eq!(call(&agent(), &url, "getSlot", &json!([])), Ok(json!(7)));
    }

    #[test]
    fn rate_limit_after_the_last_try_names_the_message() {
        let limited = (
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0",
            r#"{"jsonrpc":"2.0","error":{"code":-32005,"message":"credits exhausted"}}"#,
        );
        let url = serve(vec![limited; 5]);
        let err = call(&agent(), &url, "getSlot", &json!([])).unwrap_err();
        assert_eq!(
            err,
            r#"getSlot: HTTP 429: "credits exhausted", after 5 tries"#
        );
    }

    #[test]
    fn rate_limit_wait_obeys_retry_after_up_to_a_minute() {
        assert_eq!(rate_limit_wait(None), RATE_LIMIT_WAIT);
        assert_eq!(rate_limit_wait(Some("3")), Duration::from_secs(3));
        assert_eq!(rate_limit_wait(Some("600")), MAX_RETRY_AFTER);
        // An HTTP date is not read.
        assert_eq!(
            rate_limit_wait(Some("Wed, 21 Oct 2026 07:28:00 GMT")),
            RATE_LIMIT_WAIT
        );
    }
}
