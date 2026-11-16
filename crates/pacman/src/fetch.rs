//! GETs with a deadline and checked redirects, and conditional GETs of the sync databases.

use std::fs::{self, File};
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::process;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use jiff::fmt::rfc2822;
use tracing::debug;
use ureq::http::{Response, StatusCode, Uri, header};
use ureq::typestate::WithoutBody;
use ureq::{Body, RequestBuilder};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Mirrors use one or two.
const MAX_REDIRECTS: usize = 5;
/// A `Last-Modified` or cache mtime later than this from now is not trusted.
const FUTURE_SLACK: Duration = Duration::from_hours(24);

/// No status is an error and redirects come back too, so [`send`] can check each redirect.
pub fn agent(env_proxy: bool) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0);
    let config = if env_proxy { config } else { config.proxy(None) };
    config.build().new_agent()
}

fn get(agent: &ureq::Agent, url: &str, deadline: Instant) -> Option<RequestBuilder<WithoutBody>> {
    let left = deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())?;
    Some(
        agent
            .get(url)
            .config()
            .timeout_connect(Some(left.min(CONNECT_TIMEOUT)))
            .timeout_global(Some(left.min(REQUEST_TIMEOUT)))
            .build(),
    )
}

pub fn send(
    agent: &ureq::Agent,
    url: &str,
    deadline: Instant,
    if_modified_since: Option<&str>,
) -> Result<Response<Body>, String> {
    let origin: Uri = url
        .parse()
        .map_err(|err| format!("bad URL {url}: {err}"))?;
    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let mut request =
            get(agent, &current, deadline).ok_or_else(|| String::from("timed out"))?;
        if let Some(since) = if_modified_since {
            request = request.header(header::IF_MODIFIED_SINCE, since);
        }
        let response = request
            .call()
            .map_err(|err| err.to_string())?;
        if !matches!(
            response.status().as_u16(),
            301 | 302 | 303 | 307 | 308
        ) {
            return Ok(response);
        }
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| format!("HTTP {} without a Location", response.status()))?;
        let base: Uri = current
            .parse()
            .map_err(|err| format!("bad URL {current}: {err}"))?;
        let next = resolve(&base, location)?;
        let target: Uri = next
            .parse()
            .map_err(|err| format!("bad redirect {next}: {err}"))?;
        allowed(&origin, &target)?;
        debug!(from = current, to = next, "redirect");
        current = next;
    }
    Err(format!("more than {MAX_REDIRECTS} redirects"))
}

/// `location` against the URL that sent it, as RFC 3986 section 5.2 resolves a reference.
fn resolve(base: &Uri, location: &str) -> Result<String, String> {
    let location = location
        .split_once('#')
        .map_or(location, |(before, _)| before);
    let base_scheme = base
        .scheme_str()
        .ok_or_else(|| format!("{base} has no scheme"))?;
    let base_authority = base
        .authority()
        .map_or("", |authority| authority.as_str());
    let (scheme, rest) = match location.split_once(':') {
        Some((head, rest))
            if head
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic())
                && head
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')) =>
        {
            (Some(head), rest)
        }
        _ => (None, location),
    };
    let (authority, rest) = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after
                .find(['/', '?'])
                .unwrap_or(after.len());
            (Some(&after[..end]), &after[end..])
        }
        None => (None, rest),
    };
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    let (scheme, authority, path, query) = match (scheme, authority) {
        (Some(scheme), _) => (
            scheme,
            authority.unwrap_or(""),
            remove_dots(path),
            query,
        ),
        (None, Some(authority)) => (base_scheme, authority, remove_dots(path), query),
        (None, None) if path.is_empty() => (
            base_scheme,
            base_authority,
            base.path().to_owned(),
            query.or(base.query()),
        ),
        (None, None) if path.starts_with('/') => (
            base_scheme,
            base_authority,
            remove_dots(path),
            query,
        ),
        (None, None) => {
            let base_path = base.path();
            let dir = &base_path[..base_path
                .rfind('/')
                .map_or(0, |at| at + 1)];
            let merged = if dir.is_empty() { format!("/{path}") } else { format!("{dir}{path}") };
            (
                base_scheme,
                base_authority,
                remove_dots(&merged),
                query,
            )
        }
    };
    let path = if path.is_empty() { "/" } else { path.as_str() };
    Ok(match query {
        Some(query) => format!("{scheme}://{authority}{path}?{query}"),
        None => format!("{scheme}://{authority}{path}"),
    })
}

/// RFC 3986's `remove_dot_segments`.
fn remove_dots(path: &str) -> String {
    let absolute = path.starts_with('/');
    let segments: Vec<&str> = path
        .split('/')
        .skip(usize::from(absolute))
        .collect();
    let last = segments.len().saturating_sub(1);
    let mut out: Vec<&str> = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().enumerate() {
        match *segment {
            "." | ".." => {
                if *segment == ".." {
                    out.pop();
                }
                if index == last {
                    out.push("");
                }
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    if absolute { format!("/{joined}") } else { joined }
}

/// Only to `https`, or to `http` from an `http` server. Never to a loopback or private
/// address, unless that is the configured server's own host and port.
fn allowed(origin: &Uri, target: &Uri) -> Result<(), String> {
    let scheme = target.scheme_str().unwrap_or("");
    let secure = scheme.eq_ignore_ascii_case("https");
    let plain_ok = scheme.eq_ignore_ascii_case("http") && origin.scheme_str() == Some("http");
    if !secure && !plain_ok {
        return Err(format!(
            "a redirect to {target} is refused: {scheme} is not allowed"
        ));
    }
    let host = target
        .host()
        .ok_or_else(|| format!("a redirect to {target} has no host"))?;
    if is_local(host) {
        let same = origin
            .host()
            .is_some_and(|own| normal_host(own).eq_ignore_ascii_case(normal_host(host)))
            && port(origin) == port(target);
        if !same {
            return Err(format!(
                "a redirect to {target} is refused: a local address"
            ));
        }
    }
    Ok(())
}

fn port(uri: &Uri) -> u16 {
    uri.port_u16()
        .unwrap_or(if uri.scheme_str() == Some("https") { 443 } else { 80 })
}

fn normal_host(host: &str) -> &str {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
}

/// Whether `host` names this machine or a private network: `localhost`, or an address the
/// resolver would read as one, including forms such as `127.1` and `2130706433`.
pub fn is_local(host: &str) -> bool {
    let host = normal_host(host);
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(ip) => local_v4(ip),
            IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
                Some(ip) => local_v4(ip),
                None => {
                    let first = ip.segments()[0];
                    ip.is_loopback()
                        || ip.is_unspecified()
                        || first & 0xfe00 == 0xfc00
                        || first & 0xffc0 == 0xfe80
                }
            },
        };
    }
    // No host name has a numeric last label, so one is an address in inet_aton's forms. One
    // that does not parse is refused too.
    let last = lower.rsplit('.').next().unwrap_or("");
    if !last.is_empty() && (last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x")) {
        return inet_aton(&lower).is_none_or(local_v4);
    }
    false
}

fn local_v4(ip: Ipv4Addr) -> bool {
    ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
}

/// `a`, `a.b`, `a.b.c` or `a.b.c.d`, each part decimal, octal with a leading `0`, or hex
/// with `0x`, as `inet_aton(3)` reads them.
fn inet_aton(text: &str) -> Option<Ipv4Addr> {
    let mut parts = [0u32; 4];
    let mut count = 0;
    for part in text.split('.') {
        if count == 4 {
            return None;
        }
        let value = if let Some(hex) = part.strip_prefix("0x") {
            u32::from_str_radix(hex, 16).ok()?
        } else if part.len() > 1 && part.starts_with('0') {
            u32::from_str_radix(&part[1..], 8).ok()?
        } else {
            part.parse().ok()?
        };
        parts[count] = value;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let (head, last) = (&parts[..count - 1], parts[count - 1]);
    // The last part fills the bytes the others leave: 32 bits alone, 8 bits after three.
    let last_bits = 8 * (5 - count);
    if head.iter().any(|&part| part > 255) || (last_bits < 32 && last >= 1 << last_bits) {
        return None;
    }
    let mut value = last;
    for (index, &part) in head.iter().enumerate() {
        value |= part << (8 * (3 - index));
    }
    Some(Ipv4Addr::from(value))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetched {
    NotModified,
    Downloaded(u64),
}

/// A 200 takes `Last-Modified` as the file's mtime, or the epoch when there is none or it
/// lies in the future, so the next GET is unconditional.
pub fn fetch(
    agent: &ureq::Agent,
    url: &str,
    path: &Path,
    limit: u64,
    deadline: Instant,
) -> Result<Fetched, String> {
    let horizon = SystemTime::now() + FUTURE_SLACK;
    let since = fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .filter(|&mtime| mtime > UNIX_EPOCH && mtime <= horizon)
        .and_then(|mtime| Timestamp::try_from(mtime).ok())
        .map(|since| {
            rfc2822::DateTimePrinter::new()
                .timestamp_to_rfc9110_string(&since)
                .map_err(|err| err.to_string())
        })
        .transpose()?;
    let mut response = send(agent, url, deadline, since.as_deref())?;
    match response.status() {
        StatusCode::NOT_MODIFIED if since.is_some() => return Ok(Fetched::NotModified),
        StatusCode::OK => {}
        status => return Err(format!("HTTP {status}")),
    }
    let modified = response
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            rfc2822::DateTimeParser::new()
                .parse_timestamp(value)
                .ok()
        })
        .map_or(UNIX_EPOCH, |modified| {
            let modified = SystemTime::from(modified);
            if modified > horizon { UNIX_EPOCH } else { modified }
        });
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".part-{}", process::id()));
    let temp = Path::new(&temp);
    let written = (|| {
        let mut file = File::create(temp)?;
        let mut body = response
            .body_mut()
            .as_reader()
            .take(limit.saturating_add(1));
        let written = io::copy(&mut body, &mut file)?;
        if written > limit {
            return Err(io::Error::other(format!(
                "the database is larger than {} KiB",
                limit >> 10
            )));
        }
        file.set_modified(modified)?;
        file.sync_all()?;
        fs::rename(temp, path)?;
        Ok(written)
    })();
    match written {
        Ok(written) => Ok(Fetched::Downloaded(written)),
        Err(err) => {
            let _ = fs::remove_file(temp);
            Err(err.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(text: &str) -> Uri {
        text.parse().unwrap()
    }

    #[test]
    fn local_hosts() {
        for host in [
            "localhost",
            "LOCALHOST.",
            "a.localhost",
            "127.0.0.1",
            "127.1",
            "2130706433",
            "0x7f000001",
            "0177.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "0.0.0.0",
            "[::1]",
            "[::]",
            "[fd00::1]",
            "[fe80::1]",
            "[::ffff:127.0.0.1]",
            "1.2.3.999",
        ] {
            assert!(is_local(host), "{host}");
        }
        for host in ["mirror.example", "1.1.1.1", "[2001:db8::1]", "8.8.8.8", "a1.example"] {
            assert!(!is_local(host), "{host}");
        }
    }

    #[test]
    fn redirect_targets() {
        let base = uri("https://m.example/repo/core/os/x86_64/core.db");
        assert_eq!(
            resolve(&base, "https://n.example/x.db").unwrap(),
            "https://n.example/x.db"
        );
        assert_eq!(
            resolve(&base, "//n.example/x.db").unwrap(),
            "https://n.example/x.db"
        );
        assert_eq!(
            resolve(&base, "/x.db").unwrap(),
            "https://m.example/x.db"
        );
        assert_eq!(
            resolve(&base, "other.db").unwrap(),
            "https://m.example/repo/core/os/x86_64/other.db"
        );
        let base = uri("https://m.example/repo/os/core.db?x=1");
        for (location, expected) in [
            ("?q", "https://m.example/repo/os/core.db?q"),
            ("?", "https://m.example/repo/os/core.db?"),
            ("#frag", "https://m.example/repo/os/core.db?x=1"),
            ("../x", "https://m.example/repo/x"),
            ("../../../../x", "https://m.example/x"),
            ("./x", "https://m.example/repo/os/x"),
            (".", "https://m.example/repo/os/"),
            ("..", "https://m.example/repo/"),
            ("x/./y/../z?k", "https://m.example/repo/os/x/z?k"),
            ("/a/./b/../c", "https://m.example/a/c"),
            ("//host/x", "https://host/x"),
            ("//host:8443/a/../x?y#z", "https://host:8443/x?y"),
            ("https://n.example/a/../b", "https://n.example/b"),
            ("http://n.example", "http://n.example/"),
        ] {
            assert_eq!(
                resolve(&base, location).unwrap(),
                expected,
                "{location}"
            );
        }
    }

    #[test]
    fn redirect_rules() {
        let https = uri("https://m.example/core.db");
        let http = uri("http://m.example/core.db");
        let local = uri("http://127.0.0.1:8080/core.db");
        assert!(allowed(&https, &uri("https://n.example/core.db")).is_ok());
        assert!(
            allowed(&https, &uri("http://n.example/core.db")).is_err(),
            "no downgrade"
        );
        assert!(allowed(&http, &uri("http://n.example/core.db")).is_ok());
        assert!(allowed(&http, &uri("ftp://n.example/core.db")).is_err());
        assert!(allowed(&https, &uri("https://192.168.1.1/core.db")).is_err());
        assert!(allowed(&https, &uri("https://[::1]/core.db")).is_err());
        assert!(allowed(&local, &uri("http://127.0.0.1:8080/other.db")).is_ok());
        assert!(allowed(&local, &uri("http://127.0.0.1:8081/core.db")).is_err());
        assert!(allowed(&local, &uri("http://localhost:8080/core.db")).is_err());
    }
}
