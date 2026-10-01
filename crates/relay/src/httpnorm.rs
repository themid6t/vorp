use std::{collections::HashSet, net::IpAddr};

use http::{HeaderMap, HeaderName, HeaderValue, header};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FramingError {
    #[error("content-length and transfer-encoding are both present")]
    Ambiguous,
    #[error("conflicting content-length values")]
    ConflictingLength,
    #[error("invalid content-length value")]
    InvalidLength,
    #[error("unsupported transfer-encoding")]
    UnsupportedEncoding,
}

pub(crate) fn validate_framing(headers: &HeaderMap) -> Result<Option<u64>, FramingError> {
    let mut length = None;
    for value in headers.get_all(header::CONTENT_LENGTH) {
        let value = value.to_str().map_err(|_| FramingError::InvalidLength)?;
        for part in value.split(',') {
            let part = part.trim();
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(FramingError::InvalidLength);
            }
            let parsed = part.parse().map_err(|_| FramingError::InvalidLength)?;
            if length.is_some_and(|old| old != parsed) {
                return Err(FramingError::ConflictingLength);
            }
            length = Some(parsed);
        }
    }
    let mut transfer_encoding = false;
    for value in headers.get_all(header::TRANSFER_ENCODING) {
        let value = value
            .to_str()
            .map_err(|_| FramingError::UnsupportedEncoding)?;
        for coding in value.split(',') {
            if !coding.trim().eq_ignore_ascii_case("chunked") {
                return Err(FramingError::UnsupportedEncoding);
            }
            transfer_encoding = true;
        }
    }
    if transfer_encoding && length.is_some() {
        return Err(FramingError::Ambiguous);
    }
    Ok(length)
}

pub(crate) fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
        && connection_tokens(headers).contains(&header::UPGRADE)
}

fn connection_tokens(headers: &HeaderMap) -> HashSet<HeaderName> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| HeaderName::from_bytes(s.trim().as_bytes()).ok())
        .collect()
}

pub(crate) fn strip_hop_by_hop(headers: &mut HeaderMap) {
    for name in connection_tokens(headers) {
        headers.remove(name);
    }
    for name in [
        header::CONNECTION,
        HeaderName::from_static("proxy-connection"),
        HeaderName::from_static("keep-alive"),
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
        header::CONTENT_LENGTH,
    ] {
        headers.remove(name);
    }
}

pub(crate) fn normalize_request_headers(
    headers: &mut HeaderMap,
    host: &str,
    peer_ip: IpAddr,
) -> Result<bool, http::header::InvalidHeaderValue> {
    let websocket = is_websocket_upgrade(headers);
    strip_hop_by_hop(headers);
    if websocket {
        headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    }
    headers.insert(
        HeaderName::from_static("x-forwarded-for"),
        HeaderValue::from_str(&peer_ip.to_string())?,
    );
    headers.insert(
        HeaderName::from_static("x-forwarded-host"),
        HeaderValue::from_str(host)?,
    );
    headers.insert(header::HOST, HeaderValue::from_str(host)?);
    headers.insert(
        HeaderName::from_static("x-forwarded-proto"),
        HeaderValue::from_static("https"),
    );
    headers.remove(HeaderName::from_static("x-real-ip"));
    Ok(websocket)
}

pub(crate) fn normalize_response_headers(headers: &mut HeaderMap, websocket: bool) {
    strip_hop_by_hop(headers);
    if websocket {
        headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_table() {
        let cases = [
            (vec![("content-length", "5")], Ok(Some(5))),
            (vec![("transfer-encoding", "chunked")], Ok(None)),
            (vec![], Ok(None)),
            (
                vec![("content-length", "5"), ("transfer-encoding", "chunked")],
                Err(FramingError::Ambiguous),
            ),
            (
                vec![("content-length", "5"), ("content-length", "6")],
                Err(FramingError::ConflictingLength),
            ),
            (
                vec![("content-length", "5"), ("content-length", "5")],
                Ok(Some(5)),
            ),
            (
                vec![("content-length", "5,6")],
                Err(FramingError::ConflictingLength),
            ),
            (
                vec![("content-length", "abc")],
                Err(FramingError::InvalidLength),
            ),
            (
                vec![("content-length", "-1")],
                Err(FramingError::InvalidLength),
            ),
            (
                vec![("transfer-encoding", "gzip")],
                Err(FramingError::UnsupportedEncoding),
            ),
        ];
        for (pairs, expected) in cases {
            let mut headers = HeaderMap::new();
            for (name, value) in pairs {
                headers.append(name, HeaderValue::from_static(value));
            }
            assert_eq!(validate_framing(&headers), expected);
        }
    }

    #[test]
    fn strips_and_asserts_forwarding() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, x-secret"),
        );
        headers.insert("x-secret", HeaderValue::from_static("leak"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.1"));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
        headers.insert("x-real-ip", HeaderValue::from_static("spoof"));
        assert!(
            !normalize_request_headers(&mut headers, "app.test", "203.0.113.7".parse().unwrap())
                .unwrap()
        );
        assert!(!headers.contains_key("x-secret"));
        assert!(!headers.contains_key(header::CONNECTION));
        assert!(!headers.contains_key("x-real-ip"));
        assert_eq!(headers["x-forwarded-for"], "203.0.113.7");
        assert_eq!(headers["x-forwarded-host"], "app.test");
        assert_eq!(headers["x-forwarded-proto"], "https");
    }

    #[test]
    fn websocket_only_upgrade() {
        for (upgrade, expected) in [("websocket", true), ("h2c", false)] {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
            headers.insert(header::UPGRADE, HeaderValue::from_static(upgrade));
            assert_eq!(
                normalize_request_headers(&mut headers, "app.test", "127.0.0.1".parse().unwrap())
                    .unwrap(),
                expected
            );
            assert_eq!(headers.contains_key(header::UPGRADE), expected);
            assert_eq!(headers.contains_key(header::CONNECTION), expected);
        }
    }
}
