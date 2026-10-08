//! Merges several [`ResourcesProvider`]s into one resources capability.
//!
//! [`MergedResourcesProvider`] concatenates every composed provider's resource list, in
//! construction order, and its resource template list the same way. A resource URI listed
//! by more than one provider is a composition error, not a silent pick of one provider
//! over the other; a resource template string MAY legitimately recur across providers (two
//! providers can describe the same URI shape over disjoint data), so
//! `list_resource_templates` runs no duplicate check.
//!
//! `read_resource`, `subscribe`, and `unsubscribe` are routing operations: which composed
//! provider a URI, template, or scheme belongs to. That routing is a later task's job; this
//! module only stubs the three methods [`ResourcesProvider`] requires to compile, each
//! returning [`ErrorCode::INTERNAL_ERROR`] naming itself as unimplemented.

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
        ErrorCode, ErrorData, ListResourceTemplatesResult, ListResourcesResult,
        PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        SubscribeRequestParams, UnsubscribeRequestParams,
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
/// It adapts only the two listing methods this task implements. `read_resource`,
/// `subscribe`, and `unsubscribe` route a call to one owning inner provider rather than
/// fanning out to all of them; a later task adds their adapted counterparts alongside
/// that routing logic.
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

/// Builds the not-yet-implemented error a routing method of [`MergedResourcesProvider`]
/// answers until a later task implements routing.
fn routing_not_implemented_error(method_name: &str) -> ErrorData {
    ErrorData::new(
        ErrorCode::INTERNAL_ERROR,
        format!(
            "MergedResourcesProvider::{method_name} is not implemented yet; \
             routing lands in a later task"
        ),
        None,
    )
}

/// Composes several [`ResourcesProvider`]s into one resources capability.
///
/// `list_resources` concatenates every inner provider's resources, in construction order.
/// `list_resource_templates` concatenates every inner provider's resource templates the
/// same way. `read_resource`, `subscribe`, and `unsubscribe` are stubs: routing a call by
/// URI, template, or scheme to the owning inner provider is a later task's job.
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
        let (provider_index, inner_cursor) =
            match request.as_ref().and_then(|params| params.cursor.as_deref()) {
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
        let (provider_index, inner_cursor) =
            match request.as_ref().and_then(|params| params.cursor.as_deref()) {
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
        _request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        Err(routing_not_implemented_error("read_resource"))
    }

    async fn subscribe(
        &self,
        _request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Err(routing_not_implemented_error("subscribe"))
    }

    async fn unsubscribe(
        &self,
        _request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Err(routing_not_implemented_error("unsubscribe"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rmcp::model::{Resource, ResourceTemplate};
    use rmcp::service::Peer;

    use super::*;

    /// An in-memory [`ResourcesProvider`] stub serving fixed resource pages, by cursor, in
    /// order, and fixed resource template pages the same way.
    ///
    /// Each list's `pages` is consumed front-to-back: the first page answers a `None`
    /// cursor, and each page's own `Option<String>` cursor value is the one the *next*
    /// call must present for `Stub` to serve the following page.
    struct Stub {
        resource_pages: Mutex<Vec<(Vec<Resource>, Option<String>)>>,
        resource_template_pages: Mutex<Vec<(Vec<ResourceTemplate>, Option<String>)>>,
    }

    impl Stub {
        fn new(
            resource_pages: Vec<(Vec<Resource>, Option<String>)>,
            resource_template_pages: Vec<(Vec<ResourceTemplate>, Option<String>)>,
        ) -> Self {
            Self {
                resource_pages: Mutex::new(resource_pages),
                resource_template_pages: Mutex::new(resource_template_pages),
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
            unimplemented!("not exercised by this module's tests")
        }

        async fn subscribe(
            &self,
            _request: SubscribeRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            unimplemented!("not exercised by this module's tests")
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
}
