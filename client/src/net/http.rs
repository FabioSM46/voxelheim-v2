//! The smallest HTTP/1.1 this client can talk, and the URL shapes it needs.
//!
//! The account service speaks HTTP and this client ships no HTTP crate — the
//! dependency budget in `client/AGENTS.md` is three crates and a fourth is a
//! discussion rather than a commit. What is here is exactly the conversation
//! `net/signin.rs` has: two `POST`s with a JSON body, and one request read off a
//! loopback listener. It is not a general client and must not grow into one.
//!
//! Three details are load-bearing rather than incidental:
//!
//! - **Every request says `Connection: close`.** It makes the end of the body
//!   unambiguous even when the server sends neither a length nor a chunk header,
//!   and it means no connection is left for a second request this module would
//!   then have to manage.
//! - **All three body framings are read**, because which one arrives is Go's
//!   decision and not ours: a `Content-Length`, a `Transfer-Encoding: chunked`, or
//!   a body delimited by the close. Implementing only the one that happens to
//!   arrive today is how this breaks on a handler that grows past a buffer.
//! - **A response is bounded before it is stored.** [`MAX_RESPONSE_BYTES`] is
//!   checked as the bytes arrive, so an endless answer costs memory in the same
//!   way `frame::MAX_FRAME_SIZE` stops one on the game wire — the ordering is the
//!   security property.
//!
//! Nothing here logs and no error here quotes a body: a `finish` response carries
//! a bearer credential. See `net/json.rs`, which keeps the same rule.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// The largest response this client will hold. The service's answers are a few
/// hundred bytes; this is three orders of magnitude of headroom and still a bound.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// The largest request line a loopback listener will read before refusing.
///
/// The redirect carries a `code` and a `state` in its query, which is a couple of
/// hundred characters. Eight kilobytes is what common servers allow and is far more
/// than a redirect needs.
pub(super) const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024;

/// The port an `http` URL means when it names none.
const DEFAULT_HTTP_PORT: u16 = 80;

/// One parsed URL, in the pieces a request is built from.
///
/// Hand-rolled and narrow: it understands `scheme://host[:port][/path][?query]`
/// and nothing else — no credentials in the authority, no fragment. Every URL this
/// client parses is either one an operator typed on the command line or one the
/// account service composed, and neither has a use for the rest of RFC 3986.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Url {
    pub(super) scheme: String,
    pub(super) host: String,
    pub(super) port: u16,
    /// Always starts with `/`; `/` when the URL named no path.
    pub(super) path: String,
    /// Without the `?`. Empty when the URL had none.
    pub(super) query: String,
}

impl Url {
    /// `host:port`, which is both the `Host` header and what a socket connects to.
    ///
    /// **An IPv6 literal gets its brackets back here.** [`split_authority`] takes
    /// them off so `host` is the address alone — which is what `IpAddr` parses and
    /// what `signin::is_loopback` asks about — but `::1:7780` is neither a socket
    /// address nor a legal `Host` header. Every consumer of this string is one of
    /// those two things, so this is the one place they have to come back.
    pub(super) fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Splits a URL, or says what is wrong with it.
///
/// The error text is shown to a player, so it says what was expected rather than
/// naming a grammar.
pub(super) fn parse_url(raw: &str) -> Result<Url, String> {
    let raw = raw.trim();
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or_else(|| format!("{raw} is not a URL: it needs a scheme, as in http://host:port"))?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+')
    {
        return Err(format!("{raw} does not begin with a scheme"));
    }

    // The fragment is never sent and is dropped before anything else looks at the
    // string, exactly as a browser drops it.
    let rest = rest.split('#').next().unwrap_or("");
    let (authority, after) = match rest.find(['/', '?']) {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, ""),
    };
    let (path, query) = match after.split_once('?') {
        Some((path, query)) => (path, query),
        None => (after, ""),
    };

    if authority.contains('@') {
        return Err(format!(
            "{raw} carries credentials in its address, which this client will not send"
        ));
    }

    let (host, port) = split_authority(authority, raw)?;
    Ok(Url {
        scheme,
        host,
        port,
        path: if path.is_empty() {
            "/".to_owned()
        } else {
            path.to_owned()
        },
        query: query.to_owned(),
    })
}

/// `host`, `host:port` or `[::1]:port`.
fn split_authority(authority: &str, raw: &str) -> Result<(String, u16), String> {
    let empty = || format!("{raw} names no host");

    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| format!("{raw} has an unclosed [ in its address"))?;
        if host.is_empty() {
            return Err(empty());
        }
        let port = match after.strip_prefix(':') {
            Some(port) => port
                .parse()
                .map_err(|_| format!("{raw} does not name a port number"))?,
            None if after.is_empty() => DEFAULT_HTTP_PORT,
            None => return Err(format!("{raw} has trailing text after its address")),
        };
        return Ok((host.to_owned(), port));
    }

    match authority.rsplit_once(':') {
        Some((host, port)) => {
            if host.is_empty() {
                return Err(empty());
            }
            let port = port
                .parse()
                .map_err(|_| format!("{raw} does not name a port number"))?;
            Ok((host.to_owned(), port))
        }
        None if authority.is_empty() => Err(empty()),
        None => Ok((authority.to_owned(), DEFAULT_HTTP_PORT)),
    }
}

/// The `name=value` pairs of a query string, percent-decoded.
///
/// A `Vec` rather than a map, and duplicates are kept rather than collapsed: the
/// caller decides what a repeated `state` means, and this is not the layer to
/// decide it silently.
pub(super) fn query_pairs(query: &str) -> Result<Vec<(String, String)>, String> {
    let mut pairs = Vec::new();
    for field in query.split('&') {
        if field.is_empty() {
            continue;
        }
        let (name, value) = field.split_once('=').unwrap_or((field, ""));
        pairs.push((percent_decode(name)?, percent_decode(value)?));
    }
    Ok(pairs)
}

/// Decodes one query component: `%XX` becomes a byte and `+` becomes a space.
///
/// The result must be UTF-8. A component that is not is refused rather than
/// repaired — every value this client reads out of a query is a `code`, a `state`
/// or an error name, and none of them is text a lossy conversion would help.
///
/// **The refusal does not quote the component.** A `code` is a credential.
pub(super) fn percent_decode(raw: &str) -> Result<String, String> {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'+' => {
                out.push(b' ');
                at += 1;
            }
            b'%' => {
                let hex = bytes
                    .get(at + 1..at + 3)
                    .ok_or_else(|| "a redirect carried a truncated %-escape".to_owned())?;
                let high = hex_value(hex[0])
                    .ok_or_else(|| "a redirect carried a malformed %-escape".to_owned())?;
                let low = hex_value(hex[1])
                    .ok_or_else(|| "a redirect carried a malformed %-escape".to_owned())?;
                out.push(high * 16 + low);
                at += 3;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "a redirect carried text that is not UTF-8".to_owned())
}

const fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// One answer from the account service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Response {
    pub(super) status: u16,
    pub(super) body: String,
}

/// Sends one JSON `POST` and reads the whole answer.
///
/// `timeout` bounds each of the three phases — connect, write, read — rather than
/// the conversation as a whole, which is the granularity `TcpStream` offers.
///
/// **The body is never named in an error.** A `finish` request carries an
/// authorization code and a `finish` response carries a ticket.
pub(super) fn post_json(
    authority: &str,
    path: &str,
    body: &str,
    timeout: Duration,
) -> Result<Response, String> {
    let mut stream = connect(authority, timeout)?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|err| format!("cannot configure the connection to {authority}: {err}"))?;

    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {authority}\r\n\
         User-Agent: voxelheim-client\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json\r\n\
         Content-Length: {length}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        length = body.len()
    );
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|err| format!("cannot send a request to {authority}: {err}"))?;

    read_response(&mut stream, authority)
}

/// Sends one authenticated `GET` and reads the whole answer.
///
/// A function of its own rather than a flag on [`post_json`], because `credential` is
/// a bearer token and everything that touches one should be readable at a glance. It
/// reaches exactly one line of the request and nothing else: **no error below names
/// it, and none may**, which is the rule this module already keeps for a body.
///
/// `timeout` bounds each of the three phases — connect, write, read — which is the
/// granularity `TcpStream` offers.
pub(super) fn get_json(
    authority: &str,
    path: &str,
    credential: &str,
    timeout: Duration,
) -> Result<Response, String> {
    let mut stream = connect(authority, timeout)?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|err| format!("cannot configure the connection to {authority}: {err}"))?;

    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {authority}\r\n\
         User-Agent: voxelheim-client\r\n\
         Accept: application/json\r\n\
         Authorization: Bearer {credential}\r\n\
         Connection: close\r\n\
         \r\n"
    );
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|err| format!("cannot send a request to {authority}: {err}"))?;

    read_response(&mut stream, authority)
}

/// Connects to the first address that answers, within `timeout`.
///
/// The same shape as `session::connect`, and for the same reason: a name can
/// resolve to several addresses and `connect_timeout` takes exactly one.
fn connect(authority: &str, timeout: Duration) -> Result<TcpStream, String> {
    let candidates = authority
        .to_socket_addrs()
        .map_err(|err| format!("cannot resolve {authority}: {err}"))?;
    let mut last = None;
    for candidate in candidates {
        match TcpStream::connect_timeout(&candidate, timeout) {
            Ok(stream) => {
                // Nagle would hold a small request back waiting for a second write
                // that never comes. Best effort, as on the game socket.
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(err) => last = Some(err),
        }
    }
    Err(match last {
        Some(err) => format!("cannot reach {authority}: {err}"),
        None => format!("{authority} resolved to nothing"),
    })
}

/// Reads until the answer is complete or the peer closes.
fn read_response(stream: &mut TcpStream, authority: &str) -> Result<Response, String> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(response) = parse_response(&buffer, false)? {
            return Ok(response);
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                if buffer.len() + read > MAX_RESPONSE_BYTES {
                    return Err(format!(
                        "{authority} answered more than this client will read"
                    ));
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(format!("cannot read the answer from {authority}: {err}")),
        }
    }
    parse_response(&buffer, true)?
        .ok_or_else(|| format!("{authority} closed the connection mid-answer"))
}

/// Parses whatever has arrived so far.
///
/// `Ok(None)` means *not yet* — more bytes would complete it. At EOF the caller
/// asks again with `at_eof`, which is what turns a close-delimited body from
/// "incomplete" into "all of it".
fn parse_response(buffer: &[u8], at_eof: bool) -> Result<Option<Response>, String> {
    let Some(head_end) = find(buffer, b"\r\n\r\n") else {
        return if at_eof && !buffer.is_empty() {
            Err("the account service answered something that is not HTTP".to_owned())
        } else {
            Ok(None)
        };
    };

    let head = std::str::from_utf8(&buffer[..head_end])
        .map_err(|_| "the account service answered headers that are not text".to_owned())?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| "the account service answered no status line".to_owned())?;
    let status = parse_status(status_line)?;

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "content-length" => {
                content_length =
                    Some(value.parse().map_err(|_| {
                        "the account service answered an unreadable length".to_owned()
                    })?);
            }
            "transfer-encoding" => {
                chunked = value
                    .to_ascii_lowercase()
                    .split(',')
                    .any(|part| part.trim() == "chunked");
            }
            _ => {}
        }
    }

    let rest = &buffer[head_end + 4..];
    let body = if chunked {
        // `Transfer-Encoding` wins over `Content-Length` where both are present,
        // which is what RFC 9112 says and what every server that sends both means.
        match dechunk(rest)? {
            Some(body) => body,
            None if at_eof => {
                return Err("the account service answered a truncated body".to_owned());
            }
            None => return Ok(None),
        }
    } else if let Some(length) = content_length {
        if rest.len() < length {
            return if at_eof {
                Err("the account service answered a truncated body".to_owned())
            } else {
                Ok(None)
            };
        }
        rest[..length].to_vec()
    } else if at_eof {
        rest.to_vec()
    } else {
        return Ok(None);
    };

    Ok(Some(Response {
        status,
        body: String::from_utf8(body)
            .map_err(|_| "the account service answered a body that is not UTF-8".to_owned())?,
    }))
}

/// `HTTP/1.x NNN Reason`.
fn parse_status(line: &str) -> Result<u16, String> {
    let mut parts = line.split(' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err("the account service answered something that is not HTTP/1.1".to_owned());
    }
    parts
        .next()
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| "the account service answered no status code".to_owned())
}

/// Reassembles a chunked body, or reports that more is needed.
fn dechunk(mut rest: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut body = Vec::new();
    loop {
        let Some(line_end) = find(rest, b"\r\n") else {
            return Ok(None);
        };
        let header = std::str::from_utf8(&rest[..line_end])
            .map_err(|_| "the account service answered an unreadable chunk header".to_owned())?;
        // A chunk header may carry extensions after a `;`. Nothing here reads one.
        let size_text = header.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| "the account service answered an unreadable chunk header".to_owned())?;
        if body.len() + size > MAX_RESPONSE_BYTES {
            return Err("the account service answered more than this client will read".to_owned());
        }
        rest = &rest[line_end + 2..];
        if size == 0 {
            // The trailer section, then the final blank line. Nothing here reads a
            // trailer; what matters is that the body is complete.
            return if find(rest, b"\r\n").is_some() {
                Ok(Some(body))
            } else {
                Ok(None)
            };
        }
        if rest.len() < size + 2 {
            return Ok(None);
        }
        body.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

/// The first position of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The method and target of one request line, as a loopback listener reads it.
///
/// `GET /discord/callback?code=…&state=… HTTP/1.1`. The version is not checked: a
/// browser sends 1.1 and this listener answers one request either way.
pub(super) fn parse_request_line(line: &str) -> Result<(String, String), String> {
    let mut parts = line.split(' ');
    let method = parts
        .next()
        .filter(|method| !method.is_empty())
        .ok_or_else(|| "the browser sent a request with no method".to_owned())?;
    let target = parts
        .next()
        .filter(|target| target.starts_with('/'))
        .ok_or_else(|| "the browser sent a request with no path".to_owned())?;
    Ok((method.to_ascii_uppercase(), target.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        parse_url(raw).expect("a URL")
    }

    #[test]
    fn a_bare_host_gets_the_default_port_and_the_root_path() {
        assert_eq!(
            url("http://example.invalid"),
            Url {
                scheme: "http".to_owned(),
                host: "example.invalid".to_owned(),
                port: 80,
                path: "/".to_owned(),
                query: String::new(),
            }
        );
    }

    #[test]
    fn a_port_a_path_and_a_query_are_all_split_out() {
        let parsed = url("http://127.0.0.1:7780/v1/x?a=1&b=2");
        assert_eq!(parsed.host, "127.0.0.1");
        assert_eq!(parsed.port, 7780);
        assert_eq!(parsed.path, "/v1/x");
        assert_eq!(parsed.query, "a=1&b=2");
        assert_eq!(parsed.authority(), "127.0.0.1:7780");
    }

    #[test]
    fn an_ipv6_literal_keeps_its_brackets_out_of_the_host() {
        let parsed = url("http://[::1]:7780/callback");
        assert_eq!(parsed.host, "::1");
        assert_eq!(parsed.port, 7780);
        // And gets them back for anything that has to dial or name it: `::1:7780`
        // parses as neither a socket address nor a `Host` header.
        assert_eq!(parsed.authority(), "[::1]:7780");
        assert!(
            parsed.authority().to_socket_addrs().is_ok(),
            "an authority a socket cannot resolve is one nothing can connect to"
        );
    }

    #[test]
    fn the_scheme_is_folded_but_kept() {
        assert_eq!(url("HTTP://example.invalid").scheme, "http");
        assert_eq!(url("https://example.invalid").scheme, "https");
    }

    #[test]
    fn a_fragment_is_dropped_before_anything_reads_the_string() {
        let parsed = url("http://example.invalid/a?b=1#c");
        assert_eq!(parsed.path, "/a");
        assert_eq!(parsed.query, "b=1");
    }

    #[test]
    fn a_url_this_client_will_not_send_is_refused() {
        for raw in [
            "example.invalid",
            "http://",
            "http://:80/",
            "http://host:notaport/",
            "http://user:pass@host/",
            "http://[::1/",
        ] {
            assert!(parse_url(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn a_query_splits_into_decoded_pairs() {
        assert_eq!(
            query_pairs("code=a%2Bb&state=c+d&flag"),
            Ok(vec![
                ("code".to_owned(), "a+b".to_owned()),
                ("state".to_owned(), "c d".to_owned()),
                ("flag".to_owned(), String::new()),
            ])
        );
    }

    #[test]
    fn a_malformed_escape_is_refused_without_quoting_the_component() {
        let secret = "sup3rsecretcode";
        let err = query_pairs(&format!("code={secret}%2")).expect_err("truncated escape");
        assert!(!err.contains(secret), "{err}");
    }

    #[test]
    fn a_response_with_a_content_length_is_read_exactly() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 9\r\n\r\n{\"a\":\"b\"}trailing";
        assert_eq!(
            parse_response(raw, false),
            Ok(Some(Response {
                status: 200,
                body: "{\"a\":\"b\"}".to_owned(),
            }))
        );
    }

    #[test]
    fn a_chunked_response_is_reassembled() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"a\":\r\n4\r\n\"b\"}\r\n0\r\n\r\n";
        assert_eq!(
            parse_response(raw, false),
            Ok(Some(Response {
                status: 200,
                body: "{\"a\":\"b\"}".to_owned(),
            }))
        );
    }

    #[test]
    fn a_close_delimited_response_needs_the_close_to_be_complete() {
        let raw = b"HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\n\r\n{\"error\":\"x\"}";
        assert_eq!(parse_response(raw, false), Ok(None));
        assert_eq!(
            parse_response(raw, true),
            Ok(Some(Response {
                status: 503,
                body: "{\"error\":\"x\"}".to_owned(),
            }))
        );
    }

    #[test]
    fn a_partial_response_is_not_yet_an_answer() {
        let full = b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{\"a\":\"b\"}";
        for upto in 0..full.len() {
            assert_eq!(parse_response(&full[..upto], false), Ok(None), "{upto}");
        }
        assert!(parse_response(full, false).expect("complete").is_some());
    }

    #[test]
    fn a_truncated_body_at_eof_is_an_error_rather_than_a_short_answer() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{\"a\":";
        assert!(parse_response(raw, true).is_err());
    }

    #[test]
    fn a_reply_that_is_not_http_is_refused() {
        assert!(parse_response(b"hello\r\n\r\n", true).is_err());
        assert!(parse_response(b"HTTP/1.1\r\n\r\n", true).is_err());
        assert_eq!(parse_response(b"", true), Ok(None));
    }

    #[test]
    fn chunked_wins_over_a_length_that_is_also_present() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n";
        assert_eq!(
            parse_response(raw, false)
                .expect("complete")
                .map(|r| r.body),
            Some("hi".to_owned())
        );
    }

    #[test]
    fn a_request_line_splits_into_a_method_and_a_target() {
        assert_eq!(
            parse_request_line("GET /discord/callback?code=a HTTP/1.1"),
            Ok(("GET".to_owned(), "/discord/callback?code=a".to_owned()))
        );
        assert!(parse_request_line("GET").is_err());
        assert!(parse_request_line("GET nothing HTTP/1.1").is_err());
        assert!(parse_request_line("").is_err());
    }
}
