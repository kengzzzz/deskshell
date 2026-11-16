//! The DuckDuckGo search URL for a launcher query.

use std::fmt::Write as _;

pub const SEARCH_PREFIX: &str = "https://duckduckgo.com/?q=";

pub fn encode_uri_component(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len() * 3);
    push_encoded(&mut encoded, text);
    encoded
}

pub fn search_url(query: &str) -> String {
    let mut url = String::with_capacity(SEARCH_PREFIX.len() + query.len() * 3);
    url.push_str(SEARCH_PREFIX);
    push_encoded(&mut url, query);
    url
}

fn push_encoded(out: &mut String, text: &str) {
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_js() {
        assert_eq!(encode_uri_component(""), "");
        assert_eq!(
            encode_uri_component("a-z_A.Z!0~9*'()"),
            "a-z_A.Z!0~9*'()"
        );
        assert_eq!(
            encode_uri_component("!yt funny cats"),
            "!yt%20funny%20cats"
        );
        assert_eq!(
            encode_uri_component("a+b=c&d/e?f#g"),
            "a%2Bb%3Dc%26d%2Fe%3Ff%23g"
        );
        assert_eq!(
            encode_uri_component("ไทย"),
            "%E0%B9%84%E0%B8%97%E0%B8%A2"
        );
        assert_eq!(encode_uri_component("😀"), "%F0%9F%98%80");
        assert_eq!(
            search_url("rust lang"),
            "https://duckduckgo.com/?q=rust%20lang"
        );
    }
}
