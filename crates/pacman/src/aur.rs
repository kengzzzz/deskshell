//! The AUR RPC's `info` call.

use std::time::Instant;

use serde::Deserialize;
use ureq::http::StatusCode;

use crate::fetch;

pub const BATCH: usize = 100;
/// An answer for 100 packages is far below this.
const MAX_BODY: u64 = 16 * 1024 * 1024;

#[derive(Deserialize)]
struct Answer {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    results: Vec<Info>,
}

#[derive(Deserialize)]
pub struct Info {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Version")]
    pub version: String,
}

pub fn info(
    agent: &ureq::Agent,
    url: &str,
    names: &[&str],
    deadline: Instant,
) -> Result<Vec<Info>, String> {
    let mut found = Vec::with_capacity(names.len());
    let mut target = String::new();
    for batch in names.chunks(BATCH) {
        target.clear();
        target.push_str(url);
        for (index, name) in batch.iter().enumerate() {
            target.push(if index == 0 { '?' } else { '&' });
            target.push_str("arg%5B%5D=");
            encode(name, &mut target);
        }
        let mut response = fetch::send(agent, &target, deadline, None)?;
        if response.status() != StatusCode::OK {
            return Err(format!("HTTP {}", response.status()));
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_vec()
            .map_err(|err| err.to_string())?;
        let answer: Answer = serde_json::from_slice(&body).map_err(|err| err.to_string())?;
        if answer.kind == "error" {
            return Err(answer
                .error
                .unwrap_or_else(|| String::from("an error without a message")));
        }
        found.extend(answer.results);
    }
    Ok(found)
}

fn encode(text: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
}
