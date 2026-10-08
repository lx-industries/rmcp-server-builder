//! Opaque pagination cursor codec shared by every list-type merge.
//!
//! A merged provider composes several inner providers into one. When it paginates a
//! list, it must remember which inner provider a page's cursor belongs to, and what
//! cursor that inner provider handed back. [`encode`] packs a provider index and an
//! optional inner cursor into a single opaque [`Cursor`](rmcp::model::Cursor) string;
//! [`decode`] is its inverse.
//!
//! The encoding is explicit-length-prefixed, not a naive `:`-split, because an inner
//! cursor can itself contain `:` (nothing in the MCP spec rules this out).

/// Encodes a provider index and an optional inner cursor into one opaque cursor string.
///
/// The format is `{provider_index}:N` when `inner` is [`None`], or
/// `{provider_index}:S{byte_length}:{inner}` when `inner` is [`Some`]. The explicit byte
/// length lets [`decode`] recover `inner` verbatim even when it contains `:`.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "consumed by the pagination composition of later merge-provider tasks"
    )
)]
pub(crate) fn encode(provider_index: usize, inner: Option<&str>) -> String {
    match inner {
        None => format!("{provider_index}:N"),
        Some(inner) => format!("{provider_index}:S{}:{inner}", inner.len()),
    }
}

/// Decodes a cursor produced by [`encode`] back into a provider index and inner cursor.
///
/// # Errors
///
/// Returns [`ErrorData::invalid_params`](rmcp::model::ErrorData::invalid_params) naming
/// `cursor` when it has no valid provider index, no recognized `N`/`S` tag, or a declared
/// inner-cursor length that does not match what follows.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "consumed by the pagination composition of later merge-provider tasks"
    )
)]
pub(crate) fn decode(cursor: &str) -> Result<(usize, Option<String>), rmcp::model::ErrorData> {
    let malformed = || {
        rmcp::model::ErrorData::invalid_params(format!("malformed merge cursor: {cursor:?}"), None)
    };

    let (index_part, rest) = cursor.split_once(':').ok_or_else(malformed)?;
    let provider_index = index_part.parse::<usize>().map_err(|_| malformed())?;

    if let Some(stripped) = rest.strip_prefix('N') {
        return if stripped.is_empty() {
            Ok((provider_index, None))
        } else {
            Err(malformed())
        };
    }

    let payload = rest.strip_prefix('S').ok_or_else(malformed)?;
    let (length_part, inner) = payload.split_once(':').ok_or_else(malformed)?;
    let length = length_part.parse::<usize>().map_err(|_| malformed())?;
    if inner.len() != length {
        return Err(malformed());
    }

    Ok((provider_index, Some(inner.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_provider_index_with_an_inner_cursor() {
        let encoded = encode(1, Some("abc"));
        assert_eq!(decode(&encoded).unwrap(), (1, Some("abc".to_string())));
    }

    #[test]
    fn round_trips_a_provider_index_with_no_inner_cursor() {
        let encoded = encode(0, None);
        assert_eq!(decode(&encoded).unwrap(), (0, None));
    }

    #[test]
    fn round_trips_an_inner_cursor_containing_a_colon() {
        let encoded = encode(2, Some("page:3:of:5"));
        assert_eq!(
            decode(&encoded).unwrap(),
            (2, Some("page:3:of:5".to_string()))
        );
    }

    #[test]
    fn rejects_a_malformed_cursor() {
        assert!(decode("not-a-cursor").is_err());
    }

    #[test]
    fn rejects_a_cursor_with_an_unrecognized_tag() {
        assert!(decode("1:X").is_err());
    }

    #[test]
    fn rejects_a_cursor_whose_declared_length_does_not_match_its_payload() {
        assert!(decode("1:S10:short").is_err());
    }
}
