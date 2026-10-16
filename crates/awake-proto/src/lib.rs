//! Wire format for the awake helper socket. See spec section 3.3.

use std::fmt;

/// Maximum accepted line length in bytes, including the newline.
pub const MAX_LINE: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    SleepDisable,
    SleepEnable,
    Shutdown,
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Ok,
    Busy,
    Err(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProtoError {
    Empty,
    TooLong,
    UnknownRequest(String),
    UnknownResponse(String),
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtoError::Empty => write!(f, "empty line"),
            ProtoError::TooLong => write!(f, "line exceeds {MAX_LINE} bytes"),
            ProtoError::UnknownRequest(s) => write!(f, "unknown request: {s}"),
            ProtoError::UnknownResponse(s) => write!(f, "unknown response: {s}"),
        }
    }
}

impl std::error::Error for ProtoError {}

const REQ_SLEEP_DISABLE: &str = "sleep_disable";
const REQ_SLEEP_ENABLE: &str = "sleep_enable";
const REQ_SHUTDOWN: &str = "shutdown";
const REQ_PING: &str = "ping";

const RESP_OK: &str = "ok";
const RESP_BUSY: &str = "busy";
const RESP_ERR: &str = "err";

pub fn encode_request(r: Request) -> String {
    let word = match r {
        Request::SleepDisable => REQ_SLEEP_DISABLE,
        Request::SleepEnable => REQ_SLEEP_ENABLE,
        Request::Shutdown => REQ_SHUTDOWN,
        Request::Ping => REQ_PING,
    };
    format!("{word}\n")
}

fn clean(line: &str) -> Result<&str, ProtoError> {
    if line.len() > MAX_LINE {
        return Err(ProtoError::TooLong);
    }
    let t = line.trim();
    if t.is_empty() {
        return Err(ProtoError::Empty);
    }
    Ok(t)
}

pub fn parse_request(line: &str) -> Result<Request, ProtoError> {
    match clean(line)? {
        REQ_SLEEP_DISABLE => Ok(Request::SleepDisable),
        REQ_SLEEP_ENABLE => Ok(Request::SleepEnable),
        REQ_SHUTDOWN => Ok(Request::Shutdown),
        REQ_PING => Ok(Request::Ping),
        other => Err(ProtoError::UnknownRequest(other.to_string())),
    }
}

pub fn encode_response(r: &Response) -> String {
    match r {
        Response::Ok => format!("{RESP_OK}\n"),
        Response::Busy => format!("{RESP_BUSY}\n"),
        Response::Err(msg) => format!("{RESP_ERR} {msg}\n"),
    }
}

pub fn parse_response(line: &str) -> Result<Response, ProtoError> {
    let t = clean(line)?;
    if t == RESP_OK {
        return Ok(Response::Ok);
    }
    if t == RESP_BUSY {
        return Ok(Response::Busy);
    }
    if t == RESP_ERR {
        return Ok(Response::Err(String::new()));
    }
    if let Some(rest) = t.strip_prefix("err ") {
        return Ok(Response::Err(rest.to_string()));
    }
    Err(ProtoError::UnknownResponse(t.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        for r in [Request::SleepDisable, Request::SleepEnable, Request::Shutdown, Request::Ping] {
            let line = encode_request(r);
            assert!(line.ends_with('\n'));
            assert_eq!(parse_request(&line), Ok(r));
        }
    }

    #[test]
    fn request_words_are_stable() {
        assert_eq!(encode_request(Request::SleepDisable), "sleep_disable\n");
        assert_eq!(encode_request(Request::SleepEnable), "sleep_enable\n");
        assert_eq!(encode_request(Request::Shutdown), "shutdown\n");
        assert_eq!(encode_request(Request::Ping), "ping\n");
    }

    #[test]
    fn request_parse_tolerates_missing_newline_and_whitespace() {
        assert_eq!(parse_request("ping"), Ok(Request::Ping));
        assert_eq!(parse_request("  ping \r\n"), Ok(Request::Ping));
    }

    #[test]
    fn request_parse_rejects_bad_input() {
        assert_eq!(parse_request(""), Err(ProtoError::Empty));
        assert_eq!(parse_request("\n"), Err(ProtoError::Empty));
        assert_eq!(parse_request("reboot"), Err(ProtoError::UnknownRequest("reboot".into())));
        let long = "a".repeat(MAX_LINE + 1);
        assert_eq!(parse_request(&long), Err(ProtoError::TooLong));
    }

    #[test]
    fn response_round_trips() {
        for r in [Response::Ok, Response::Busy, Response::Err("no owner".into())] {
            let line = encode_response(&r);
            assert!(line.ends_with('\n'));
            assert_eq!(parse_response(&line), Ok(r));
        }
    }

    #[test]
    fn response_words_are_stable() {
        assert_eq!(encode_response(&Response::Ok), "ok\n");
        assert_eq!(encode_response(&Response::Busy), "busy\n");
        assert_eq!(encode_response(&Response::Err("x y".into())), "err x y\n");
    }

    #[test]
    fn response_parse_rejects_bad_input() {
        assert_eq!(parse_response(""), Err(ProtoError::Empty));
        assert_eq!(parse_response("fine"), Err(ProtoError::UnknownResponse("fine".into())));
        assert_eq!(parse_response("err"), Ok(Response::Err(String::new())));
    }

    #[test]
    fn errors_display() {
        assert_eq!(ProtoError::Empty.to_string(), "empty line");
        assert_eq!(ProtoError::TooLong.to_string(), "line exceeds 256 bytes");
        assert_eq!(ProtoError::UnknownRequest("x".into()).to_string(), "unknown request: x");
        assert_eq!(ProtoError::UnknownResponse("x".into()).to_string(), "unknown response: x");
    }
}
