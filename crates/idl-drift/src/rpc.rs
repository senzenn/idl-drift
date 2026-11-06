//! One JSON-RPC request over HTTP, with the retry rules that `idl-drift` and `idl-verify`
//! share.

use std::time::Duration;

use serde_json::{json, Value};

use crate::markdown::escape_control;

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
/// The public mainnet RPC took 49 s to list 10 signatures of a program with little traffic.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(2);
/// A host that does not accept the connection in this time is likely down.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// An HTTP agent for [`call`]: a 10 s limit to connect, a 2 min limit per request, and HTTP
/// errors as responses.
#[must_use]
pub fn agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .http_status_as_error(false)
            .build(),
    )
}

/// Send one JSON-RPC request and return its `result`. A 429, a 5xx, a lost connection and
/// a cut-off answer are tried again, up to 5 tries in total. A timeout is tried again only
/// one time.
///
/// # Errors
///
/// `<method>: <detail>`. The text never holds `url`, which can hold an API key.
pub fn call(agent: &ureq::Agent, url: &str, method: &str, params: &Value) -> Result<Value, String> {
    let fail = |detail: String| format!("{method}: {detail}");
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let mut last = String::new();
    let mut timed_out = false;
    for attempt in 0..ATTEMPTS {
        let wait = match agent.post(url).send_json(&body) {
            Err(e) if retryable(&e, timed_out) => {
                timed_out |= matches!(e, ureq::Error::Timeout(_));
                last = transport_text(&e);
                backoff(attempt)
            }
            Err(e) => return Err(fail(stop_text(&e, attempt))),
            Ok(mut response) => match response.status().as_u16() {
                429 => {
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    last = error_text(&mut response, &mut timed_out, attempt).map_err(fail)?;
                    rate_limit_wait(retry_after.as_deref())
                }
                500.. => {
                    last = error_text(&mut response, &mut timed_out, attempt).map_err(fail)?;
                    backoff(attempt)
                }
                200 => {
                    // `read_json` reports the size limit as a JSON error, which is tried again.
                    let read = response
                        .body_mut()
                        .with_config()
                        .limit(MAX_RESPONSE_BYTES)
                        .read_to_vec()
                        .and_then(|bytes| {
                            serde_json::from_slice::<Value>(&bytes).map_err(ureq::Error::Json)
                        });
                    match read {
                        Ok(mut reply) => {
                            if let Some(error) = reply.get("error").filter(|e| !e.is_null()) {
                                return Err(fail(escape_control(&error.to_string())));
                            }
                            return reply
                                .as_object_mut()
                                .map(|fields| fields.remove("result").unwrap_or(Value::Null))
                                .ok_or_else(|| fail("the answer is not a JSON-RPC object".into()));
                        }
                        Err(e) if retryable(&e, timed_out) => {
                            timed_out |= matches!(e, ureq::Error::Timeout(_));
                            last = transport_text(&e);
                            backoff(attempt)
                        }
                        Err(e) => return Err(fail(stop_text(&e, attempt))),
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
fn retryable(e: &ureq::Error, timed_out: bool) -> bool {
    match e {
        ureq::Error::Io(io) => io.kind() != std::io::ErrorKind::ConnectionRefused,
        // Each try can wait the full timeout, and a host that timed out two times is likely down.
        ureq::Error::Timeout(_) => !timed_out,
        ureq::Error::ConnectionFailed | ureq::Error::Json(_) => true,
        _ => false,
    }
}

/// The text of `e` when it stops the request at try `attempt`, counted from 0.
fn stop_text(e: &ureq::Error, attempt: u32) -> String {
    match attempt {
        0 => transport_text(e),
        n => format!("{}, after {} tries", transport_text(e), n + 1),
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

/// `HTTP <status>`, and `: <message>` from a JSON-RPC error body, such as Helius's "credits
/// exhausted". A timeout while it reads the body is a timeout of the try too.
fn error_text(
    response: &mut ureq::http::Response<ureq::Body>,
    timed_out: &mut bool,
    attempt: u32,
) -> Result<String, String> {
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_ERROR_BYTES)
        .read_json::<Value>();
    match body {
        Err(e @ ureq::Error::Timeout(_)) if *timed_out => {
            Err(format!("HTTP {status}, then {}", stop_text(&e, attempt)))
        }
        Err(e @ ureq::Error::Timeout(_)) => {
            *timed_out = true;
            Ok(format!("HTTP {status}, then {}", transport_text(&e)))
        }
        body => Ok(body
            .ok()
            .and_then(|v| v.pointer("/error/message").cloned())
            .map_or_else(
                || format!("HTTP {status}"),
                |m| format!("HTTP {status}: {}", escape_control(&m.to_string())),
            )),
    }
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
    fn an_answer_over_the_size_limit_is_not_tried_again() {
        let limit = usize::try_from(MAX_RESPONSE_BYTES).unwrap();
        let big: &'static str = format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":"{}"}}"#,
            "a".repeat(limit)
        )
        .leak();
        let url = serve(vec![
            ("HTTP/1.1 200 OK", big),
            ("HTTP/1.1 200 OK", r#"{"jsonrpc":"2.0","id":1,"result":7}"#),
        ]);
        assert_eq!(
            call(&agent(), &url, "getSlot", &json!([])),
            Err("getSlot: the response body is larger than request limit: 33554432".into())
        );
    }

    #[test]
    fn an_answer_that_is_not_an_object_is_an_error() {
        let url = serve(vec![("HTTP/1.1 200 OK", "[1]")]);
        assert_eq!(
            call(&agent(), &url, "getSlot", &json!([])),
            Err("getSlot: the answer is not a JSON-RPC object".into())
        );
    }

    #[test]
    fn a_null_error_is_no_error() {
        let url = serve(vec![(
            "HTTP/1.1 200 OK",
            r#"{"jsonrpc":"2.0","id":1,"result":7,"error":null}"#,
        )]);
        assert_eq!(call(&agent(), &url, "getSlot", &json!([])), Ok(json!(7)));
    }

    #[test]
    fn control_characters_in_an_rpc_error_are_escaped() {
        let url = serve(vec![(
            "HTTP/1.1 200 OK",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"message\":\"\u{9b}2J\u{7f}\"}}",
        )]);
        assert_eq!(
            call(&agent(), &url, "getSlot", &json!([])).unwrap_err(),
            r#"getSlot: {"message":"\u{9b}2J\u{7f}"}"#
        );
    }

    #[test]
    fn a_timeout_is_tried_again_only_one_time() {
        // The listener never accepts, so each connection waits in the backlog for an answer.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let quick = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_millis(200)))
                .build(),
        );
        assert_eq!(
            call(&quick, &url, "getSlot", &json!([])),
            Err("getSlot: timeout: global, after 2 tries".into())
        );
        listener.set_nonblocking(true).unwrap();
        assert_eq!(std::iter::from_fn(|| listener.accept().ok()).count(), 2);
    }

    #[test]
    fn a_timeout_in_the_body_of_an_error_answer_is_a_timeout() {
        // The server sends the head of a 503 and never the body that it announces.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let tries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = tries.clone();
        std::thread::spawn(move || {
            let mut open = Vec::new();
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let head = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 100\r\n\r\n";
                stream.write_all(head.as_bytes()).unwrap();
                open.push(stream);
            }
        });
        let quick = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_millis(200)))
                .http_status_as_error(false)
                .build(),
        );
        assert_eq!(
            call(&quick, &url, "getSlot", &json!([])),
            Err("getSlot: HTTP 503, then timeout: global, after 2 tries".into())
        );
        assert_eq!(tries.load(std::sync::atomic::Ordering::SeqCst), 2);
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
