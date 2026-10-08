//! Merges several [`ResourcesProvider`]s into one resources capability.
//!
//! [`MergedResourcesProvider`] concatenates every composed provider's resource list, in
//! construction order, and its resource template list the same way. A resource URI listed
//! by more than one provider is a composition error, not a silent pick of one provider
//! over the other; a resource template string MAY legitimately recur across providers (two
//! providers can describe the same URI shape over disjoint data), so
//! `list_resource_templates` runs no duplicate check.
//!
//! `read_resource`, `subscribe`, and `unsubscribe` are routing operations: each resolves
//! which composed provider a URI belongs to, then delegates the call to that provider.
//! Routing tries, in order: (1) an exact URI listed by exactly one provider's
//! `list_resources`; (2) failing that, a URI matching exactly one provider's resource
//! template (level-1 RFC 6570, see [`template_matches`]); (3) failing that, the sole
//! provider that owns the URI's scheme, derived from the schemes of every provider's
//! listed resources and templates. Two or more candidates at any step, or zero at the
//! last one, is a routing error naming the URI (and the candidates, where there is more
//! than one).

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "consumed once src/lib.rs re-exports MergedResourcesProvider (task 9); \
                  exercised directly by this module's own tests until then"
    )
)]

use std::collections::HashMap;
use std::pin::Pin;

use rmcp::{
    model::{
        ErrorData, ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, SubscribeRequestParams,
        UnsubscribeRequestParams,
    },
    service::{RequestContext, RoleServer},
};

use crate::providers::ResourcesProvider;

use super::cursor;

/// Object-safe adapter over [`ResourcesProvider`], boxing its futures.
///
/// `ResourcesProvider`'s methods are written as `-> impl Future<Output = ...> + Send`
/// (return-position impl trait), which is not object-safe: `Box<dyn ResourcesProvider>`
/// cannot be named. This sealed trait is implemented for every `T: ResourcesProvider`
/// through a blanket implementation below, boxing each future so
/// [`MergedResourcesProvider`] can store `Vec<Box<dyn DynResourcesProvider>>` internally,
/// without changing `ResourcesProvider`'s public, unboxed shape.
///
/// It adapts every [`ResourcesProvider`] method: the two listing methods, concatenated
/// across providers, and `read_resource`/`subscribe`/`unsubscribe`, routed to one owning
/// inner provider rather than fanned out to all of them.
trait DynResourcesProvider: Send + Sync {
    /// Object-safe counterpart of [`ResourcesProvider::list_resources`].
    fn list_resources<'provider>(
        &'provider self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<ListResourcesResult, ErrorData>> + Send + 'provider>>;

    /// Object-safe counterpart of [`ResourcesProvider::list_resource_templates`].
    fn list_resource_templates<'provider>(
        &'provider self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<
        Box<dyn Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + Send + 'provider>,
    >;

    /// Object-safe counterpart of [`ResourcesProvider::read_resource`].
    fn read_resource<'provider>(
        &'provider self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<ReadResourceResponse, ErrorData>> + Send + 'provider>>;

    /// Object-safe counterpart of [`ResourcesProvider::subscribe`].
    fn subscribe<'provider>(
        &'provider self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>>;

    /// Object-safe counterpart of [`ResourcesProvider::unsubscribe`].
    fn unsubscribe<'provider>(
        &'provider self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>>;
}

impl<T: ResourcesProvider> DynResourcesProvider for T {
    fn list_resources<'provider>(
        &'provider self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<ListResourcesResult, ErrorData>> + Send + 'provider>>
    {
        Box::pin(ResourcesProvider::list_resources(self, request, context))
    }

    fn list_resource_templates<'provider>(
        &'provider self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<
        Box<dyn Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + Send + 'provider>,
    > {
        Box::pin(ResourcesProvider::list_resource_templates(
            self, request, context,
        ))
    }

    fn read_resource<'provider>(
        &'provider self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<ReadResourceResponse, ErrorData>> + Send + 'provider>>
    {
        Box::pin(ResourcesProvider::read_resource(self, request, context))
    }

    fn subscribe<'provider>(
        &'provider self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>> {
        Box::pin(ResourcesProvider::subscribe(self, request, context))
    }

    fn unsubscribe<'provider>(
        &'provider self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>> {
        Box::pin(ResourcesProvider::unsubscribe(self, request, context))
    }
}

/// Builds the duplicate-resource-URI error [`MergedResourcesProvider`] answers when
/// `resource_uri` is listed by both `first_provider_index` and `second_provider_index`.
fn duplicate_resource_uri_error(
    resource_uri: &str,
    first_provider_index: usize,
    second_provider_index: usize,
) -> ErrorData {
    ErrorData::invalid_params(
        format!(
            "resource {resource_uri:?} is listed by both provider {first_provider_index} and \
             provider {second_provider_index}"
        ),
        None,
    )
}

/// Tests whether `uri` matches the level-1 [RFC 6570] `template`.
///
/// Splits `template` and `uri` on `/` and compares them segment by segment: they match
/// when both have the same segment count, and every template segment is either
/// identical to the corresponding `uri` segment, or a single `{name}` placeholder
/// matching any non-empty `uri` segment. A segment is a placeholder only when `name`
/// carries no RFC 6570 operator sigil (`+ # . / ; ? &`) right after `{`; a segment
/// using one is not a placeholder this matcher expands, so it compares as a literal,
/// matching only a `uri` segment with the exact same text, sigil included.
///
/// # Limitations
///
/// This implements level-1 matching only: a placeholder consumes
/// exactly one `/`-delimited segment. It does not support multi-segment matching
/// (`{+name}`, reserved-expansion `{#name}`) or any other level-2/level-3 RFC 6570
/// operator; a template using one is compared as a literal segment, so it matches only
/// a `uri` segment with the exact same text, placeholder syntax included.
///
/// [RFC 6570]: https://www.rfc-editor.org/rfc/rfc6570
fn template_matches(template: &str, uri: &str) -> bool {
    let mut template_segments = template.split('/');
    let mut uri_segments = uri.split('/');
    loop {
        match (template_segments.next(), uri_segments.next()) {
            (Some(template_segment), Some(uri_segment)) => {
                let placeholder_name = template_segment
                    .strip_prefix('{')
                    .and_then(|rest| rest.strip_suffix('}'))
                    .filter(|name| !name.is_empty());
                // A segment is a placeholder only with no RFC 6570 operator sigil
                // (`+ # . / ; ? &`) right after `{`: one of those marks a level-2/3
                // expansion this matcher does not implement (see this function's doc
                // comment), so the segment compares as a literal instead.
                let is_placeholder = placeholder_name
                    .is_some_and(|name| !name.starts_with(['+', '#', '.', '/', ';', '?', '&']));
                if is_placeholder {
                    if uri_segment.is_empty() {
                        return false;
                    }
                } else if template_segment != uri_segment {
                    return false;
                }
            }
            (None, None) => return true,
            _ => return false,
        }
    }
}

/// Extracts the scheme from `uri`: the substring before its first `"://"`.
///
/// Returns `None` when `uri` carries no `"://"` separator.
/// `scheme_of("blob://foo/bar")` is `Some("blob")`.
fn scheme_of(uri: &str) -> Option<&str> {
    uri.split_once("://").map(|(scheme, _)| scheme)
}

/// Builds the ambiguity error [`MergedResourcesProvider::resolve_provider_index_for_uri`]
/// answers when two or more resource templates, possibly listed by different providers,
/// match `uri`.
fn ambiguous_template_match_error(uri: &str, candidates: &[(usize, &str)]) -> ErrorData {
    let candidate_list = candidates
        .iter()
        .map(|(provider_index, template)| {
            format!("provider {provider_index} template {template:?}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    ErrorData::invalid_params(
        format!("resource URI {uri:?} matches more than one resource template: {candidate_list}"),
        None,
    )
}

/// Builds the scheme-ownership error [`MergedResourcesProvider::resolve_provider_index_for_uri`]
/// answers when, after exact-URI and template routing both miss, zero or more than one
/// composed provider owns `uri`'s `scheme`.
///
/// `owning_providers` holds every owning provider's 0-indexed construction order; an
/// empty slice means no provider owns `scheme`.
fn scheme_ownership_error(uri: &str, scheme: &str, owning_providers: &[usize]) -> ErrorData {
    let message = if owning_providers.is_empty() {
        format!(
            "resource URI {uri:?} matches no composed provider's resources or \
             templates, and no provider owns scheme {scheme:?}"
        )
    } else {
        let candidate_list = owning_providers
            .iter()
            .map(|provider_index| format!("provider {provider_index}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "resource URI {uri:?} matches no composed provider's resources or \
             templates, and scheme {scheme:?} is owned by more than one provider: \
             {candidate_list}"
        )
    };
    ErrorData::invalid_params(message, None)
}

/// Builds the error [`MergedResourcesProvider::resolve_provider_index_for_uri`] answers
/// when `uri` carries no `"://"` scheme separator, so scheme-ownership routing (the
/// routing order's last step) cannot apply.
fn no_scheme_error(uri: &str) -> ErrorData {
    ErrorData::invalid_params(
        format!("resource URI {uri:?} has no \"://\" scheme separator to route by"),
        None,
    )
}

#[cfg(test)]
mod template_matches_tests {
    use super::template_matches;

    #[test]
    fn template_with_no_placeholder_matches_only_its_exact_self() {
        assert!(template_matches("blob://foo/bar", "blob://foo/bar"));
        assert!(!template_matches("blob://foo/bar", "blob://foo/baz"));
    }

    #[test]
    fn template_with_one_placeholder_matches_a_differing_final_segment() {
        assert!(template_matches("blob://{id}", "blob://42"));
        assert!(template_matches("blob://{id}", "blob://anything-else"));
    }

    #[test]
    fn template_does_not_match_a_uri_with_a_different_segment_count() {
        assert!(!template_matches("blob://{id}", "blob://a/b"));
        assert!(!template_matches("blob://a/b", "blob://a"));
    }

    #[test]
    fn template_does_not_match_when_a_literal_segment_differs() {
        assert!(!template_matches("blob://foo/{id}", "blob://bar/42"));
    }

    #[test]
    fn placeholder_does_not_match_an_empty_segment() {
        assert!(!template_matches("blob://{id}", "blob://"));
    }

    #[test]
    fn a_segment_carrying_an_rfc_6570_operator_sigil_compares_as_a_literal() {
        assert!(!template_matches("blob://x/{+id}", "blob://x/42"));
        assert!(template_matches("blob://x/{+id}", "blob://x/{+id}"));
    }
}

/// Composes several [`ResourcesProvider`]s into one resources capability.
///
/// `list_resources` concatenates every inner provider's resources, in construction order.
/// `list_resource_templates` concatenates every inner provider's resource templates the
/// same way. `read_resource`, `subscribe`, and `unsubscribe` route a call to one owning
/// inner provider: see [`MergedResourcesProvider::resolve_provider_index_for_uri`] for the
/// routing order.
///
/// # Duplicate resource URIs
///
/// A resource URI listed by more than one inner provider is a composition error:
/// [`list_resources`](ResourcesProvider::list_resources) answers
/// `Err(ErrorData::invalid_params(...))` naming the URI and the two providers, by their
/// 0-indexed construction order, rather than silently preferring one provider over the
/// other.
///
/// # Resource templates are exempt
///
/// Two inner providers MAY legitimately list the exact same resource template string: a
/// template is a URI shape, not an identity, and two providers can validly describe the
/// same shape over disjoint underlying data. `list_resource_templates` runs no duplicate
/// check.
pub struct MergedResourcesProvider {
    /// The composed providers, in listing order.
    providers: Vec<Box<dyn DynResourcesProvider>>,
}

impl MergedResourcesProvider {
    /// Starts an empty composition.
    ///
    /// Add providers with [`MergedResourcesProvider::with_provider`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Adds `provider` to the composition, after every provider added so far.
    ///
    /// A provider's 0-indexed position among `with_provider` calls is the index
    /// [`MergedResourcesProvider`] uses in a duplicate-resource-URI error and in the merge
    /// cursor it hands out.
    #[must_use]
    pub fn with_provider<T: ResourcesProvider>(mut self, provider: T) -> Self {
        self.providers.push(Box::new(provider));
        self
    }

    /// Drains every composed provider's full resource list, following each provider's own
    /// pagination until it answers `next_cursor: None`.
    ///
    /// Returns each resource's URI tagged with the 0-indexed provider that listed it, in
    /// provider-then-page order.
    async fn drain_resource_uris(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Vec<(usize, String)>, ErrorData> {
        let mut uris = Vec::new();
        for (provider_index, provider) in self.providers.iter().enumerate() {
            let mut inner_cursor = None;
            let mut seen_inner_cursors = std::collections::HashSet::new();
            for page_count in 0.. {
                let page = provider
                    .list_resources(
                        Some(PaginatedRequestParams::default().with_cursor(inner_cursor)),
                        context.clone(),
                    )
                    .await?;
                uris.extend(
                    page.resources
                        .iter()
                        .map(|resource| (provider_index, resource.uri.clone())),
                );
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
        Ok(uris)
    }

    /// Maps every composed provider's resource URIs to the provider that listed them.
    ///
    /// # Errors
    ///
    /// Returns [`duplicate_resource_uri_error`] for the first resource URI found listed by
    /// two different providers (by 0-indexed construction order).
    async fn index_resource_uris_by_provider(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<HashMap<String, usize>, ErrorData> {
        let uris = self.drain_resource_uris(context).await?;
        let mut owner_by_uri = HashMap::new();
        for (provider_index, resource_uri) in uris {
            match owner_by_uri.get(&resource_uri) {
                Some(&owning_index) if owning_index != provider_index => {
                    return Err(duplicate_resource_uri_error(
                        &resource_uri,
                        owning_index,
                        provider_index,
                    ));
                }
                _ => {
                    owner_by_uri.insert(resource_uri, provider_index);
                }
            }
        }
        Ok(owner_by_uri)
    }

    /// Drains every composed provider's full resource template list, following each
    /// provider's own pagination until it answers `next_cursor: None`.
    ///
    /// Returns each resource template string tagged with the 0-indexed provider that
    /// listed it, in provider-then-page order. Unlike
    /// [`MergedResourcesProvider::drain_resource_uris`], this runs no duplicate check: two
    /// providers MAY legitimately list the same template string.
    async fn drain_resource_templates(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Vec<(usize, String)>, ErrorData> {
        let mut templates = Vec::new();
        for (provider_index, provider) in self.providers.iter().enumerate() {
            let mut inner_cursor = None;
            let mut seen_inner_cursors = std::collections::HashSet::new();
            for page_count in 0.. {
                let page = provider
                    .list_resource_templates(
                        Some(PaginatedRequestParams::default().with_cursor(inner_cursor)),
                        context.clone(),
                    )
                    .await?;
                templates.extend(page.resource_templates.iter().map(|resource_template| {
                    (provider_index, resource_template.uri_template.clone())
                }));
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
        Ok(templates)
    }

    /// Resolves which composed provider owns `uri`, for `read_resource`, `subscribe`, and
    /// `unsubscribe` to delegate to.
    ///
    /// Tries, in order:
    ///
    /// 1. An exact URI listed by exactly one provider's `list_resources` routes there.
    /// 2. Failing that, a URI matching one or more resource templates of exactly one
    ///    provider (level-1 RFC 6570, via [`template_matches`]) routes there: ambiguity
    ///    is counted by provider, not by template.
    /// 3. Failing both, the provider that owns `uri`'s scheme routes there: a provider
    ///    owns a scheme when at least one of its listed resources or templates uses it.
    ///
    /// # Errors
    ///
    /// Step 1 returns [`duplicate_resource_uri_error`] when `uri` is listed by two
    /// providers (expected to have already been rejected by a prior `list_resources`
    /// call; see that function's doc comment on the transient-state case). Step 2
    /// returns [`ambiguous_template_match_error`] when matching templates span two or
    /// more distinct providers. Step 3 returns [`no_scheme_error`] when `uri`
    /// has no `"://"` separator, or [`scheme_ownership_error`] when zero or more than one
    /// provider owns the scheme.
    async fn resolve_provider_index_for_uri(
        &self,
        uri: &str,
        context: &RequestContext<RoleServer>,
    ) -> Result<usize, ErrorData> {
        let owner_by_uri = self.index_resource_uris_by_provider(context).await?;
        if let Some(&provider_index) = owner_by_uri.get(uri) {
            return Ok(provider_index);
        }

        let templates = self.drain_resource_templates(context).await?;
        let matching_templates: Vec<(usize, &str)> = templates
            .iter()
            .filter(|(_, template)| template_matches(template, uri))
            .map(|(provider_index, template)| (*provider_index, template.as_str()))
            .collect();
        // Ambiguity is counted by provider, not by template: a URI matched only by
        // templates of one provider (even several of them) routes to that provider.
        // Templates of two or more distinct providers still make the match ambiguous.
        let mut matching_provider_indices: Vec<usize> =
            matching_templates.iter().map(|(index, _)| *index).collect();
        matching_provider_indices.sort_unstable();
        matching_provider_indices.dedup();
        match matching_provider_indices.as_slice() {
            [only_provider_index] => return Ok(*only_provider_index),
            [] => {}
            _ => return Err(ambiguous_template_match_error(uri, &matching_templates)),
        }

        let scheme = scheme_of(uri).ok_or_else(|| no_scheme_error(uri))?;
        let mut owning_providers: Vec<usize> = owner_by_uri
            .iter()
            .filter(|(resource_uri, _)| scheme_of(resource_uri) == Some(scheme))
            .map(|(_, &provider_index)| provider_index)
            .chain(
                templates
                    .iter()
                    .filter(|(_, template)| scheme_of(template) == Some(scheme))
                    .map(|(provider_index, _)| *provider_index),
            )
            .collect();
        owning_providers.sort_unstable();
        owning_providers.dedup();

        match owning_providers.as_slice() {
            [only_owner] => Ok(*only_owner),
            owners => Err(scheme_ownership_error(uri, scheme, owners)),
        }
    }
}

impl Default for MergedResourcesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourcesProvider for MergedResourcesProvider {
    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let cursor = request.as_ref().and_then(|params| params.cursor.as_deref());
        if cursor.is_none() && self.providers.is_empty() {
            // An empty composition has no provider to page into: answer an empty page
            // directly rather than treating provider index 0 as in range.
            return Ok(ListResourcesResult::with_all_items(Vec::new()));
        }
        let (provider_index, inner_cursor) = match cursor {
            Some(cursor) => cursor::decode(cursor)?,
            None => (0, None),
        };

        // Every call re-validates the whole composition: a duplicate introduced by a
        // provider whose own resource list changed between calls must surface here too.
        self.index_resource_uris_by_provider(&context).await?;

        let provider_count = self.providers.len();
        let provider = self.providers.get(provider_index).ok_or_else(|| {
            ErrorData::invalid_params(
                format!(
                    "merge cursor names provider {provider_index}, but only \
                     {provider_count} providers are composed"
                ),
                None,
            )
        })?;

        let page = provider
            .list_resources(
                Some(PaginatedRequestParams::default().with_cursor(inner_cursor)),
                context,
            )
            .await?;

        let next_cursor = match page.next_cursor {
            Some(inner_next) => Some(cursor::encode(provider_index, Some(&inner_next))),
            None if provider_index + 1 < provider_count => {
                Some(cursor::encode(provider_index + 1, None))
            }
            None => None,
        };

        let mut result = ListResourcesResult::with_all_items(page.resources);
        result.next_cursor = next_cursor;
        Ok(result)
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let cursor = request.as_ref().and_then(|params| params.cursor.as_deref());
        if cursor.is_none() && self.providers.is_empty() {
            // An empty composition has no provider to page into: answer an empty page
            // directly rather than treating provider index 0 as in range.
            return Ok(ListResourceTemplatesResult::with_all_items(Vec::new()));
        }
        let (provider_index, inner_cursor) = match cursor {
            Some(cursor) => cursor::decode(cursor)?,
            None => (0, None),
        };

        let provider_count = self.providers.len();
        let provider = self.providers.get(provider_index).ok_or_else(|| {
            ErrorData::invalid_params(
                format!(
                    "merge cursor names provider {provider_index}, but only \
                     {provider_count} providers are composed"
                ),
                None,
            )
        })?;

        let page = provider
            .list_resource_templates(
                Some(PaginatedRequestParams::default().with_cursor(inner_cursor)),
                context,
            )
            .await?;

        let next_cursor = match page.next_cursor {
            Some(inner_next) => Some(cursor::encode(provider_index, Some(&inner_next))),
            None if provider_index + 1 < provider_count => {
                Some(cursor::encode(provider_index + 1, None))
            }
            None => None,
        };

        let mut result = ListResourceTemplatesResult::with_all_items(page.resource_templates);
        result.next_cursor = next_cursor;
        Ok(result)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let provider_index = self
            .resolve_provider_index_for_uri(&request.uri, &context)
            .await?;
        self.providers[provider_index]
            .read_resource(request, context)
            .await
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let provider_index = self
            .resolve_provider_index_for_uri(&request.uri, &context)
            .await?;
        self.providers[provider_index]
            .subscribe(request, context)
            .await
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let provider_index = self
            .resolve_provider_index_for_uri(&request.uri, &context)
            .await?;
        self.providers[provider_index]
            .unsubscribe(request, context)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use rmcp::model::{ReadResourceResult, Resource, ResourceTemplate};
    use rmcp::service::Peer;

    use super::*;

    /// An in-memory [`ResourcesProvider`] stub serving fixed resource pages, by cursor, in
    /// order, and fixed resource template pages the same way.
    ///
    /// Each list's `pages` is consumed front-to-back: the first page answers a `None`
    /// cursor, and each page's own `Option<String>` cursor value is the one the *next*
    /// call must present for `Stub` to serve the following page.
    ///
    /// `read_resource` and `subscribe` ignore the request's URI (this module's routing
    /// tests assert routing by checking which `Stub` was reached, via [`Stub::named`] and
    /// [`Stub::with_call_log`], not by the content either method answers).
    struct Stub {
        /// This stub's name, pushed onto `call_log` by `read_resource` and `subscribe`.
        name: &'static str,
        resource_pages: Mutex<Vec<(Vec<Resource>, Option<String>)>>,
        resource_template_pages: Mutex<Vec<(Vec<ResourceTemplate>, Option<String>)>>,
        /// Shared call log; `read_resource` and `subscribe` push [`Stub::name`] onto it
        /// when set, so a test can assert which stub a routed call reached.
        call_log: Option<Arc<Mutex<Vec<&'static str>>>>,
    }

    impl Stub {
        fn new(
            resource_pages: Vec<(Vec<Resource>, Option<String>)>,
            resource_template_pages: Vec<(Vec<ResourceTemplate>, Option<String>)>,
        ) -> Self {
            Self {
                name: "stub",
                resource_pages: Mutex::new(resource_pages),
                resource_template_pages: Mutex::new(resource_template_pages),
                call_log: None,
            }
        }

        /// A stub with every resource on a single page (`next_cursor: None`) and no
        /// resource templates.
        fn single_page(resources: Vec<Resource>) -> Self {
            Self::new(vec![(resources, None)], vec![(Vec::new(), None)])
        }

        /// A stub with every resource template on a single page (`next_cursor: None`) and
        /// no resources.
        fn single_template_page(resource_templates: Vec<ResourceTemplate>) -> Self {
            Self::new(vec![(Vec::new(), None)], vec![(resource_templates, None)])
        }

        /// Names this stub, read back from `call_log` entries it pushes.
        fn named(mut self, name: &'static str) -> Self {
            self.name = name;
            self
        }

        /// Shares `call_log` with this stub: `read_resource` and `subscribe` push
        /// [`Stub::name`] onto it when called.
        fn with_call_log(mut self, call_log: Arc<Mutex<Vec<&'static str>>>) -> Self {
            self.call_log = Some(call_log);
            self
        }
    }

    fn page_index<T>(
        pages: &[(Vec<T>, Option<String>)],
        requested_cursor: &Option<String>,
    ) -> usize {
        match requested_cursor {
            None => 0,
            Some(cursor) => pages
                .iter()
                .position(|(_, next_cursor)| next_cursor.as_deref() == Some(cursor.as_str()))
                .map(|index| index + 1)
                .expect("test passes back a cursor this stub produced"),
        }
    }

    impl ResourcesProvider for Stub {
        async fn list_resources(
            &self,
            request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListResourcesResult, ErrorData> {
            let requested_cursor = request.and_then(|params| params.cursor);
            let pages = self.resource_pages.lock().expect("stub pages lock");
            let index = page_index(&pages, &requested_cursor);
            let (resources, next_cursor) = pages
                .get(index)
                .cloned()
                .expect("test does not request a page past this stub's last one");
            let mut result = ListResourcesResult::with_all_items(resources);
            result.next_cursor = next_cursor;
            Ok(result)
        }

        async fn list_resource_templates(
            &self,
            request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListResourceTemplatesResult, ErrorData> {
            let requested_cursor = request.and_then(|params| params.cursor);
            let pages = self
                .resource_template_pages
                .lock()
                .expect("stub pages lock");
            let index = page_index(&pages, &requested_cursor);
            let (resource_templates, next_cursor) = pages
                .get(index)
                .cloned()
                .expect("test does not request a page past this stub's last one");
            let mut result = ListResourceTemplatesResult::with_all_items(resource_templates);
            result.next_cursor = next_cursor;
            Ok(result)
        }

        async fn read_resource(
            &self,
            _request: ReadResourceRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<ReadResourceResponse, ErrorData> {
            if let Some(call_log) = &self.call_log {
                call_log.lock().expect("call log lock").push(self.name);
            }
            Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
                Vec::new(),
            )))
        }

        async fn subscribe(
            &self,
            _request: SubscribeRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            if let Some(call_log) = &self.call_log {
                call_log.lock().expect("call log lock").push(self.name);
            }
            Ok(())
        }

        async fn unsubscribe(
            &self,
            _request: UnsubscribeRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            unimplemented!("not exercised by this module's tests")
        }
    }

    fn resource(uri: &'static str) -> Resource {
        Resource::new(uri, uri)
    }

    fn resource_template(uri_template: &'static str) -> ResourceTemplate {
        ResourceTemplate::new(uri_template, uri_template)
    }

    /// A [`RequestContext<RoleServer>`] usable in a pure unit test.
    ///
    /// `rmcp`'s `Peer` has no public constructor outside a live client/server
    /// handshake, so this mints one over an in-memory duplex transport and then
    /// discards the connection: `MergedResourcesProvider` and `Stub` never read from
    /// `context.peer`, they only forward it, so a torn-down peer is a valid stand-in.
    async fn test_context() -> RequestContext<RoleServer> {
        use rmcp::ServiceExt;
        use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        let server = crate::ServerBuilder::new()
            .info(Implementation::new("merge-resources-test", "0.0.0"))
            .build();
        let server_task = tokio::spawn(async move {
            let running = server.serve(server_transport).await.expect("serve server");
            let peer: Peer<RoleServer> = running.peer().clone();
            running.waiting().await.expect("server stops");
            peer
        });

        let client_configuration = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("merge-resources-test-client", "0.0.0"),
        );
        let client = client_configuration
            .serve(client_transport)
            .await
            .expect("initialize client");
        client.cancel().await.expect("cancel client");
        let peer = server_task.await.expect("server task");

        RequestContext::new(rmcp::model::RequestId::Number(0), peer)
    }

    #[tokio::test]
    async fn an_empty_composition_lists_no_resources_and_no_next_cursor() {
        let merged = MergedResourcesProvider::new();

        let result = ResourcesProvider::list_resources(&merged, None, test_context().await)
            .await
            .expect("an empty composition lists nothing instead of erroring");

        assert!(result.resources.is_empty());
        assert_eq!(result.next_cursor, None);
    }

    #[tokio::test]
    async fn an_empty_composition_lists_no_resource_templates_and_no_next_cursor() {
        let merged = MergedResourcesProvider::new();

        let result =
            ResourcesProvider::list_resource_templates(&merged, None, test_context().await)
                .await
                .expect("an empty composition lists nothing instead of erroring");

        assert!(result.resource_templates.is_empty());
        assert_eq!(result.next_cursor, None);
    }

    #[tokio::test]
    async fn a_cursor_naming_a_missing_provider_on_an_empty_resource_composition_is_still_an_error()
    {
        let merged = MergedResourcesProvider::new();
        let request =
            Some(PaginatedRequestParams::default().with_cursor(Some(cursor::encode(0, None))));

        let error = ResourcesProvider::list_resources(&merged, request, test_context().await)
            .await
            .expect_err("a supplied cursor naming a missing provider stays an error");

        assert!(error.message.contains('0'), "{error:?}");
    }

    #[tokio::test]
    async fn concatenates_non_overlapping_single_page_providers() {
        // `MergedResourcesProvider::list_resources` answers exactly one inner provider's
        // page per call (see its doc comment), so a client that pages from `None` through
        // every `next_cursor` sees all 4 resources, concatenated in provider order, across
        // the two merge-level pages this composition produces (one per provider).
        let merged = MergedResourcesProvider::new()
            .with_provider(Stub::single_page(vec![
                resource("a://1"),
                resource("a://2"),
            ]))
            .with_provider(Stub::single_page(vec![
                resource("b://1"),
                resource("b://2"),
            ]));

        let mut all_resource_uris = Vec::new();
        let mut cursor = None;
        loop {
            let request = cursor
                .take()
                .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
            let result = ResourcesProvider::list_resources(&merged, request, test_context().await)
                .await
                .expect("list_resources succeeds");
            all_resource_uris.extend(result.resources.iter().map(|resource| resource.uri.clone()));
            match result.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        assert_eq!(all_resource_uris, vec!["a://1", "a://2", "b://1", "b://2"]);
    }

    #[tokio::test]
    async fn paginates_across_two_providers_with_two_pages_each() {
        let merged = MergedResourcesProvider::new()
            .with_provider(Stub::new(
                vec![
                    (vec![resource("a://1")], Some("a-page-2".to_string())),
                    (vec![resource("a://2")], None),
                ],
                vec![(Vec::new(), None)],
            ))
            .with_provider(Stub::new(
                vec![
                    (vec![resource("b://1")], Some("b-page-2".to_string())),
                    (vec![resource("b://2")], None),
                ],
                vec![(Vec::new(), None)],
            ));

        let mut seen_resource_uris = Vec::new();
        let mut cursor = None;
        for _ in 0..4 {
            let context = test_context().await;
            let request = Some(PaginatedRequestParams::default().with_cursor(cursor.clone()));
            let result = ResourcesProvider::list_resources(&merged, request, context)
                .await
                .expect("list_resources succeeds");
            assert_eq!(
                result.resources.len(),
                1,
                "one provider-page per merge page"
            );
            seen_resource_uris.push(result.resources[0].uri.clone());
            cursor = result.next_cursor;
        }

        assert_eq!(seen_resource_uris, vec!["a://1", "a://2", "b://1", "b://2"]);
        assert_eq!(cursor, None, "the fourth call ends pagination");
    }

    #[tokio::test]
    async fn rejects_a_resource_uri_shared_by_two_providers() {
        let merged = MergedResourcesProvider::new()
            .with_provider(Stub::single_page(vec![resource("shared://1")]))
            .with_provider(Stub::single_page(vec![resource("shared://1")]));

        let error = ResourcesProvider::list_resources(&merged, None, test_context().await)
            .await
            .expect_err("list_resources reports the duplicate");

        assert!(error.message.contains("shared://1"), "{error:?}");
    }

    /// A [`ResourcesProvider`] whose `list_resources` always answers the same
    /// `next_cursor`, so a drain that does not guard against a repeated cursor never
    /// terminates. `list_resource_templates` answers one empty, terminated page.
    struct RepeatingResourceCursorStub;

    impl ResourcesProvider for RepeatingResourceCursorStub {
        async fn list_resources(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListResourcesResult, ErrorData> {
            let mut result = ListResourcesResult::with_all_items(vec![resource("a://1")]);
            result.next_cursor = Some("same-cursor".to_string());
            Ok(result)
        }

        async fn list_resource_templates(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListResourceTemplatesResult, ErrorData> {
            Ok(ListResourceTemplatesResult::with_all_items(Vec::new()))
        }

        async fn read_resource(
            &self,
            _request: ReadResourceRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<ReadResourceResponse, ErrorData> {
            unimplemented!("not exercised by this test")
        }

        async fn subscribe(
            &self,
            _request: SubscribeRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            unimplemented!("not exercised by this test")
        }

        async fn unsubscribe(
            &self,
            _request: UnsubscribeRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            unimplemented!("not exercised by this test")
        }
    }

    /// A [`ResourcesProvider`] whose `list_resource_templates` always answers the
    /// same `next_cursor`, so a drain that does not guard against a repeated cursor
    /// never terminates. `list_resources` answers one empty, terminated page.
    struct RepeatingTemplateCursorStub;

    impl ResourcesProvider for RepeatingTemplateCursorStub {
        async fn list_resources(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListResourcesResult, ErrorData> {
            Ok(ListResourcesResult::with_all_items(Vec::new()))
        }

        async fn list_resource_templates(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListResourceTemplatesResult, ErrorData> {
            let mut result =
                ListResourceTemplatesResult::with_all_items(vec![resource_template("a://{id}")]);
            result.next_cursor = Some("same-cursor".to_string());
            Ok(result)
        }

        async fn read_resource(
            &self,
            _request: ReadResourceRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<ReadResourceResponse, ErrorData> {
            unimplemented!("not exercised by this test")
        }

        async fn subscribe(
            &self,
            _request: SubscribeRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            unimplemented!("not exercised by this test")
        }

        async fn unsubscribe(
            &self,
            _request: UnsubscribeRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            unimplemented!("not exercised by this test")
        }
    }

    #[tokio::test]
    async fn a_provider_whose_resource_next_cursor_always_repeats_is_refused_and_does_not_hang() {
        let merged = MergedResourcesProvider::new().with_provider(RepeatingResourceCursorStub);

        let error = ResourcesProvider::list_resources(&merged, None, test_context().await)
            .await
            .expect_err("a repeating cursor ends the drain instead of hanging");

        assert!(error.message.contains("provider 0"), "{error:?}");
    }

    #[tokio::test]
    async fn a_provider_whose_template_next_cursor_always_repeats_is_refused_and_does_not_hang() {
        let merged = MergedResourcesProvider::new().with_provider(RepeatingTemplateCursorStub);

        let error = ResourcesProvider::read_resource(
            &merged,
            ReadResourceRequestParams::new("unmatched://x"),
            test_context().await,
        )
        .await
        .expect_err("a repeating template cursor ends the drain instead of hanging");

        assert!(error.message.contains("provider 0"), "{error:?}");
    }

    #[tokio::test]
    async fn concatenates_resource_templates_sharing_the_same_uri_template_with_no_error() {
        // A resource template is a URI shape, not an identity, so two providers
        // legitimately describing the same shape is not a composition error.
        let merged = MergedResourcesProvider::new()
            .with_provider(Stub::single_template_page(vec![resource_template(
                "shared://{id}",
            )]))
            .with_provider(Stub::single_template_page(vec![resource_template(
                "shared://{id}",
            )]));

        let mut all_uri_templates = Vec::new();
        let mut cursor = None;
        loop {
            let request = cursor
                .take()
                .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
            let result =
                ResourcesProvider::list_resource_templates(&merged, request, test_context().await)
                    .await
                    .expect("list_resource_templates succeeds even with a shared template string");
            all_uri_templates.extend(
                result
                    .resource_templates
                    .iter()
                    .map(|resource_template| resource_template.uri_template.clone()),
            );
            match result.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        assert_eq!(
            all_uri_templates,
            vec!["shared://{id}", "shared://{id}"],
            "both copies of the shared template concatenate, with no duplicate error"
        );
    }

    #[tokio::test]
    async fn concatenates_non_overlapping_resource_templates_across_providers() {
        let merged = MergedResourcesProvider::new()
            .with_provider(Stub::single_template_page(vec![resource_template(
                "a://{id}",
            )]))
            .with_provider(Stub::single_template_page(vec![resource_template(
                "b://{id}",
            )]));

        let mut all_uri_templates = Vec::new();
        let mut cursor = None;
        loop {
            let request = cursor
                .take()
                .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
            let result =
                ResourcesProvider::list_resource_templates(&merged, request, test_context().await)
                    .await
                    .expect("list_resource_templates succeeds");
            all_uri_templates.extend(
                result
                    .resource_templates
                    .iter()
                    .map(|resource_template| resource_template.uri_template.clone()),
            );
            match result.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        assert_eq!(all_uri_templates, vec!["a://{id}", "b://{id}"]);
    }

    #[tokio::test]
    async fn exact_uri_listed_by_one_provider_routes_read_resource_there() {
        let call_log = Arc::new(Mutex::new(Vec::new()));
        let merged = MergedResourcesProvider::new()
            .with_provider(
                Stub::single_page(vec![resource("a://1")])
                    .named("a")
                    .with_call_log(call_log.clone()),
            )
            .with_provider(
                Stub::single_page(vec![resource("b://1")])
                    .named("b")
                    .with_call_log(call_log.clone()),
            );

        ResourcesProvider::read_resource(
            &merged,
            ReadResourceRequestParams::new("b://1"),
            test_context().await,
        )
        .await
        .expect("read_resource succeeds");

        assert_eq!(*call_log.lock().expect("call log lock"), vec!["b"]);
    }

    #[tokio::test]
    async fn uri_matching_only_one_providers_template_routes_read_resource_there() {
        let call_log = Arc::new(Mutex::new(Vec::new()));
        let merged = MergedResourcesProvider::new()
            .with_provider(
                Stub::single_template_page(vec![resource_template("a://{id}")])
                    .named("a")
                    .with_call_log(call_log.clone()),
            )
            .with_provider(
                Stub::single_template_page(vec![resource_template("b://{id}")])
                    .named("b")
                    .with_call_log(call_log.clone()),
            );

        ResourcesProvider::read_resource(
            &merged,
            ReadResourceRequestParams::new("a://42"),
            test_context().await,
        )
        .await
        .expect("read_resource succeeds");

        assert_eq!(*call_log.lock().expect("call log lock"), vec!["a"]);
    }

    #[tokio::test]
    async fn uri_whose_scheme_only_one_provider_uses_routes_there_via_scheme_fallback() {
        // "c://x" is listed by neither provider and matches no template, but provider
        // "a" is the only one whose other listings use scheme "c" (via "c://known"), so
        // scheme-ownership routing (step 3) reaches it.
        let call_log = Arc::new(Mutex::new(Vec::new()));
        let merged = MergedResourcesProvider::new()
            .with_provider(
                Stub::single_page(vec![resource("c://known")])
                    .named("a")
                    .with_call_log(call_log.clone()),
            )
            .with_provider(
                Stub::single_page(vec![resource("b://1")])
                    .named("b")
                    .with_call_log(call_log.clone()),
            );

        ResourcesProvider::read_resource(
            &merged,
            ReadResourceRequestParams::new("c://x"),
            test_context().await,
        )
        .await
        .expect("read_resource succeeds via scheme fallback");

        assert_eq!(*call_log.lock().expect("call log lock"), vec!["a"]);
    }

    #[tokio::test]
    async fn one_provider_listing_two_matching_templates_is_not_ambiguous() {
        // S4: ambiguity counts providers, not templates. Provider 0 lists both
        // "blob://{collection}/{id}" and "blob://users/{id}"; both match
        // "blob://users/42", but both come from the same provider.
        let call_log = Arc::new(Mutex::new(Vec::new()));
        let merged = MergedResourcesProvider::new()
            .with_provider(
                Stub::single_template_page(vec![
                    resource_template("blob://{collection}/{id}"),
                    resource_template("blob://users/{id}"),
                ])
                .named("a")
                .with_call_log(call_log.clone()),
            )
            .with_provider(
                Stub::single_template_page(vec![resource_template("other://{id}")])
                    .named("b")
                    .with_call_log(call_log.clone()),
            );

        ResourcesProvider::read_resource(
            &merged,
            ReadResourceRequestParams::new("blob://users/42"),
            test_context().await,
        )
        .await
        .expect("a single provider's two matching templates are not ambiguous");

        assert_eq!(*call_log.lock().expect("call log lock"), vec!["a"]);
    }

    #[tokio::test]
    async fn scheme_used_by_two_providers_with_no_exact_or_template_match_is_an_error() {
        let merged = MergedResourcesProvider::new()
            .with_provider(Stub::single_page(vec![resource("c://1")]))
            .with_provider(Stub::single_page(vec![resource("c://2")]));

        let error = ResourcesProvider::read_resource(
            &merged,
            ReadResourceRequestParams::new("c://x"),
            test_context().await,
        )
        .await
        .expect_err("ambiguous scheme ownership is a routing error");

        assert!(error.message.contains("provider 0"), "{error:?}");
        assert!(error.message.contains("provider 1"), "{error:?}");
    }

    #[tokio::test]
    async fn subscribe_reaches_the_same_provider_read_resource_would() {
        let call_log = Arc::new(Mutex::new(Vec::new()));
        let merged = MergedResourcesProvider::new()
            .with_provider(
                Stub::single_page(vec![resource("a://1")])
                    .named("a")
                    .with_call_log(call_log.clone()),
            )
            .with_provider(
                Stub::single_page(vec![resource("b://1")])
                    .named("b")
                    .with_call_log(call_log.clone()),
            );

        ResourcesProvider::read_resource(
            &merged,
            ReadResourceRequestParams::new("b://1"),
            test_context().await,
        )
        .await
        .expect("read_resource succeeds");

        ResourcesProvider::subscribe(
            &merged,
            SubscribeRequestParams::new("b://1"),
            test_context().await,
        )
        .await
        .expect("subscribe succeeds");

        assert_eq!(*call_log.lock().expect("call log lock"), vec!["b", "b"]);
    }
}
