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

/// Page cap for a full drain of one composed provider's list, or one completion
/// source's scan, before it is refused as non-terminating.
///
/// A provider that never answers `next_cursor: None` would otherwise drain forever;
/// this cap turns that into an error after a generous but finite number of pages.
pub(crate) const MAX_DRAIN_PAGES: usize = 10_000;

/// Guards one step of a full drain against a non-terminating provider.
///
/// Call this once per page, after reading that page's `next_cursor`, with the inner
/// cursor the page just answered (the one the *next* call would present) and the set
/// of inner cursors already seen in this drain. `provider_index` and `page_count` are
/// folded into the error so it names where the drain stalled.
///
/// # Errors
///
/// Returns [`rmcp::model::ErrorData::internal_error`] naming `provider_index` and
/// `next_inner_cursor` when that cursor was already seen earlier in this drain.
/// Returns the same kind of error naming `provider_index` and [`MAX_DRAIN_PAGES`]
/// when `page_count` reaches the cap.
pub(crate) fn guard_drain_progress(
    provider_index: usize,
    page_count: usize,
    next_inner_cursor: &str,
    seen_inner_cursors: &mut std::collections::HashSet<String>,
) -> Result<(), rmcp::model::ErrorData> {
    if !seen_inner_cursors.insert(next_inner_cursor.to_string()) {
        return Err(rmcp::model::ErrorData::internal_error(
            format!(
                "provider {provider_index}: drain cursor {next_inner_cursor:?} repeats \
                 a cursor already seen in this drain"
            ),
            None,
        ));
    }
    if page_count + 1 >= MAX_DRAIN_PAGES {
        return Err(rmcp::model::ErrorData::internal_error(
            format!(
                "provider {provider_index}: drain did not terminate within \
                 {MAX_DRAIN_PAGES} pages"
            ),
            None,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn guard_drain_progress_rejects_a_repeated_inner_cursor() {
        let mut seen = HashSet::new();
        guard_drain_progress(0, 0, "a", &mut seen).expect("first sighting of \"a\" passes");

        let error = guard_drain_progress(0, 1, "a", &mut seen)
            .expect_err("a repeated inner cursor ends the drain");

        assert_eq!(error.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert!(error.message.contains("provider 0"), "{error:?}");
        assert!(error.message.contains("\"a\""), "{error:?}");
    }

    #[test]
    fn guard_drain_progress_rejects_reaching_the_page_cap() {
        let mut seen = HashSet::new();
        let error = guard_drain_progress(2, MAX_DRAIN_PAGES - 1, "fresh", &mut seen)
            .expect_err("reaching the page cap ends the drain");

        assert_eq!(error.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert!(error.message.contains("provider 2"), "{error:?}");
        assert!(
            error.message.contains(&MAX_DRAIN_PAGES.to_string()),
            "{error:?}"
        );
    }

    #[test]
    fn guard_drain_progress_accepts_distinct_cursors_under_the_cap() {
        let mut seen = HashSet::new();
        guard_drain_progress(0, 0, "a", &mut seen).expect("distinct cursor under the cap passes");
        guard_drain_progress(0, 1, "b", &mut seen).expect("distinct cursor under the cap passes");
    }

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
