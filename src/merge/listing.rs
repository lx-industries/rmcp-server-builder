//! Generic drain, index, and one-page skeleton shared by the tools, prompts, and
//! resources merges.
//!
//! Each of those capabilities answers one page per `list_*` call
//! ([`list_one_page`]), drains every composed provider's full item list to check for
//! a duplicate key before serving that page ([`drain`], [`index_by_provider`]), and
//! composes a merge-level pagination cursor the same way (`super::cursor`). This
//! module holds that shape once, parameterized by the item type `T` the capability
//! lists (`Tool`, `Prompt`, `Resource`, `ResourceTemplate`) and, for `index_by_provider`,
//! the key type `K` a duplicate is named by (a tool name, a prompt name, a resource
//! URI). `list_resource_templates` uses [`list_one_page`] but not
//! [`index_by_provider`]: two providers MAY legitimately list the same resource
//! template string, so it runs no duplicate check.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::pin::Pin;

use rmcp::model::ErrorData;

use super::cursor;

/// One page of `T` from one composed provider, abstracting over the per-capability
/// list result types (`ListToolsResult`, `ListPromptsResult`, `ListResourcesResult`,
/// `ListResourceTemplatesResult`).
pub(crate) struct Page<T> {
    pub(crate) items: Vec<T>,
    pub(crate) next_cursor: Option<String>,
}

/// Boxed, object-safe future for one provider's one-page list call.
///
/// Each merge module already adapts its provider trait into an object-safe `Dyn*`
/// counterpart (boxing its futures, since the public provider traits return
/// `impl Future` and are therefore not object-safe); this module takes that adapted
/// call as a plain closure over `(provider_index, inner_cursor)` instead of a second
/// trait, so it stays parameterized by the item type alone.
pub(crate) type ListPageFuture<'provider, T> =
    Pin<Box<dyn Future<Output = Result<Page<T>, ErrorData>> + Send + 'provider>>;

/// Builds the duplicate-key error a merged `list_*` or call-routing method answers
/// when `key` (a tool name, a prompt name, or a resource URI — named by `kind`, e.g.
/// `"tool"`, `"prompt"`, `"resource"`) is listed by both `first_provider_index` and
/// `second_provider_index`.
pub(crate) fn duplicate_key_error(
    kind: &str,
    key: &str,
    first_provider_index: usize,
    second_provider_index: usize,
) -> ErrorData {
    ErrorData::invalid_params(
        format!(
            "{kind} {key:?} is listed by both provider {first_provider_index} and \
             provider {second_provider_index}"
        ),
        None,
    )
}

/// Builds the out-of-range error [`list_one_page`] answers when a merge cursor names
/// a provider index past `provider_count`.
fn provider_index_out_of_range_error(provider_index: usize, provider_count: usize) -> ErrorData {
    ErrorData::invalid_params(
        format!(
            "merge cursor names provider {provider_index}, but only \
             {provider_count} providers are composed"
        ),
        None,
    )
}

/// Drains every composed provider's full item list, following each provider's own
/// pagination (via `list_page`) until it answers `next_cursor: None`, guarded against
/// a non-terminating provider by [`cursor::guard_drain_progress`].
///
/// Returns each item tagged with the 0-indexed provider that listed it, in
/// provider-then-page order.
pub(crate) async fn drain<'provider, T>(
    provider_count: usize,
    mut list_page: impl FnMut(usize, Option<String>) -> ListPageFuture<'provider, T>,
) -> Result<Vec<(usize, T)>, ErrorData> {
    let mut all = Vec::new();
    for provider_index in 0..provider_count {
        let mut inner_cursor = None;
        let mut seen_inner_cursors = HashSet::new();
        for page_count in 0.. {
            let page = list_page(provider_index, inner_cursor.take()).await?;
            all.extend(page.items.into_iter().map(|item| (provider_index, item)));
            match page.next_cursor {
                Some(next) => {
                    cursor::guard_drain_progress(
                        provider_index,
                        page_count,
                        &next,
                        &mut seen_inner_cursors,
                    )?;
                    inner_cursor = Some(next);
                }
                None => break,
            }
        }
    }
    Ok(all)
}

/// Maps every composed provider's `key_of(item)` to the provider that listed it, from
/// an already-[`drain`]ed item list.
///
/// # Errors
///
/// Returns [`duplicate_key_error`] (tagged `kind`) for the first key found listed by
/// two different providers (by 0-indexed construction order).
pub(crate) fn index_by_provider<T, K: Eq + Hash + Clone>(
    drained: Vec<(usize, T)>,
    kind: &str,
    key_of: impl Fn(&T) -> K,
    key_display: impl Fn(&K) -> String,
) -> Result<HashMap<K, usize>, ErrorData> {
    let mut owner_by_key = HashMap::new();
    for (provider_index, item) in &drained {
        let key = key_of(item);
        match owner_by_key.get(&key) {
            Some(&owning_index) if owning_index != *provider_index => {
                return Err(duplicate_key_error(
                    kind,
                    &key_display(&key),
                    owning_index,
                    *provider_index,
                ));
            }
            _ => {
                owner_by_key.insert(key, *provider_index);
            }
        }
    }
    Ok(owner_by_key)
}

/// Serves exactly one composed provider's one page, resolved from `cursor` the same
/// way every merge-level `list_*` method does: a cursor-free request starts at
/// provider 0; an empty composition with no cursor answers an empty page directly,
/// since there is no provider to page into; a decoded cursor names the provider and
/// inner cursor to resume. The merge-level `next_cursor` composes that provider's own
/// `next_cursor`, or, once it is exhausted, the next provider's index with no inner
/// cursor, or `None` past the last provider.
///
/// # Errors
///
/// Returns [`cursor::decode`]'s error for a malformed cursor, or
/// [`provider_index_out_of_range_error`] when the cursor names a provider past
/// `provider_count`.
pub(crate) async fn list_one_page<'provider, T>(
    merge_cursor: Option<&str>,
    provider_count: usize,
    list_page: impl FnOnce(usize, Option<String>) -> ListPageFuture<'provider, T>,
) -> Result<(Vec<T>, Option<String>), ErrorData> {
    if merge_cursor.is_none() && provider_count == 0 {
        return Ok((Vec::new(), None));
    }
    let (provider_index, inner_cursor) = match merge_cursor {
        Some(merge_cursor) => cursor::decode(merge_cursor)?,
        None => (0, None),
    };
    if provider_index >= provider_count {
        return Err(provider_index_out_of_range_error(
            provider_index,
            provider_count,
        ));
    }

    let page = list_page(provider_index, inner_cursor).await?;

    let next_cursor = match page.next_cursor {
        Some(inner_next) => Some(cursor::encode(provider_index, Some(&inner_next))),
        None if provider_index + 1 < provider_count => {
            Some(cursor::encode(provider_index + 1, None))
        }
        None => None,
    };

    Ok((page.items, next_cursor))
}
