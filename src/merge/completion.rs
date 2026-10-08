//! Merges several [`CompletionProvider`]s into one completion capability.
//!
//! [`MergedCompletionProvider`] routes a `complete` call by the
//! [`CompleteRequestParams::r#ref`](rmcp::model::CompleteRequestParams) it carries: a
//! prompt-name reference ([`Reference::Prompt`]) is routed to whichever composed prompt
//! source lists that prompt name, and a resource-template-URI reference
//! ([`Reference::Resource`]) is routed to whichever composed resource source lists that
//! template URI. A reference matching no composed source's listing answers
//! `CompleteResult::default()`, matching `rmcp`'s own default `complete` body (see
//! `handler/server.rs`, around line 406 of rmcp 3.5.1): an unmatched reference is not a
//! composition error.

use std::pin::Pin;

use rmcp::{
    model::{
        CompleteRequestParams, CompleteResult, ErrorData, ListPromptsResult,
        ListResourceTemplatesResult, PaginatedRequestParams, Reference,
    },
    service::{RequestContext, RoleServer},
};

use crate::providers::{CompletionProvider, PromptsProvider, ResourcesProvider};

/// Object-safe adapter over a value that is both a [`PromptsProvider`] and a
/// [`CompletionProvider`], boxing its futures.
///
/// [`MergedCompletionProvider`] composes one slot per prompt source: the same
/// underlying value answers both which prompts it lists (`list_prompts`, used only for
/// routing) and the actual completion (`complete`). Storing the two capabilities behind
/// one boxed value, rather than two separate lists correlated by index, makes that
/// pairing structural: see [`MergedCompletionProvider`]'s doc comment.
trait DynPromptCompletionSource: Send + Sync {
    /// Object-safe counterpart of [`PromptsProvider::list_prompts`], used only to
    /// resolve whether this source owns a prompt-name reference.
    fn list_prompts<'source>(
        &'source self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<ListPromptsResult, ErrorData>> + Send + 'source>>;

    /// Object-safe counterpart of [`CompletionProvider::complete`].
    fn complete<'source>(
        &'source self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<CompleteResult, ErrorData>> + Send + 'source>>;
}

impl<T: PromptsProvider + CompletionProvider> DynPromptCompletionSource for T {
    fn list_prompts<'source>(
        &'source self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<ListPromptsResult, ErrorData>> + Send + 'source>> {
        Box::pin(PromptsProvider::list_prompts(self, request, context))
    }

    fn complete<'source>(
        &'source self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<CompleteResult, ErrorData>> + Send + 'source>> {
        Box::pin(CompletionProvider::complete(self, request, context))
    }
}

/// Object-safe adapter over a value that is both a [`ResourcesProvider`] and a
/// [`CompletionProvider`], boxing its futures.
///
/// Mirrors [`DynPromptCompletionSource`]: one boxed value per resource source answers
/// both which resource templates it lists (`list_resource_templates`, used only for
/// routing) and the actual completion (`complete`).
trait DynResourceCompletionSource: Send + Sync {
    /// Object-safe counterpart of [`ResourcesProvider::list_resource_templates`], used
    /// only to resolve whether this source owns a resource-template reference.
    fn list_resource_templates<'source>(
        &'source self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<
        Box<dyn Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + Send + 'source>,
    >;

    /// Object-safe counterpart of [`CompletionProvider::complete`].
    fn complete<'source>(
        &'source self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<CompleteResult, ErrorData>> + Send + 'source>>;
}

impl<T: ResourcesProvider + CompletionProvider> DynResourceCompletionSource for T {
    fn list_resource_templates<'source>(
        &'source self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<
        Box<dyn Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + Send + 'source>,
    > {
        Box::pin(ResourcesProvider::list_resource_templates(
            self, request, context,
        ))
    }

    fn complete<'source>(
        &'source self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<CompleteResult, ErrorData>> + Send + 'source>> {
        Box::pin(CompletionProvider::complete(self, request, context))
    }
}

/// Drains `source`'s full prompt list, following its own pagination until it answers
/// `next_cursor: None`, and reports whether `prompt_name` appears in it.
async fn source_lists_prompt_name(
    source_index: usize,
    source: &dyn DynPromptCompletionSource,
    prompt_name: &str,
    context: &RequestContext<RoleServer>,
) -> Result<bool, ErrorData> {
    let mut inner_cursor = None;
    let mut seen_inner_cursors = std::collections::HashSet::new();
    for page_count in 0.. {
        let page = source
            .list_prompts(
                Some(PaginatedRequestParams::default().with_cursor(inner_cursor)),
                context.clone(),
            )
            .await?;
        if page.prompts.iter().any(|prompt| prompt.name == prompt_name) {
            return Ok(true);
        }
        match page.next_cursor {
            Some(next) => {
                super::cursor::guard_drain_progress(
                    source_index,
                    page_count,
                    &next,
                    &mut seen_inner_cursors,
                )?;
                inner_cursor = Some(next);
            }
            None => return Ok(false),
        }
    }
    unreachable!("the for loop's range has no upper bound")
}

/// Drains `source`'s full resource template list, following its own pagination until it
/// answers `next_cursor: None`, and reports whether `template_uri` appears in it.
async fn source_lists_resource_template(
    source_index: usize,
    source: &dyn DynResourceCompletionSource,
    template_uri: &str,
    context: &RequestContext<RoleServer>,
) -> Result<bool, ErrorData> {
    let mut inner_cursor = None;
    let mut seen_inner_cursors = std::collections::HashSet::new();
    for page_count in 0.. {
        let page = source
            .list_resource_templates(
                Some(PaginatedRequestParams::default().with_cursor(inner_cursor)),
                context.clone(),
            )
            .await?;
        if page
            .resource_templates
            .iter()
            .any(|resource_template| resource_template.uri_template == template_uri)
        {
            return Ok(true);
        }
        match page.next_cursor {
            Some(next) => {
                super::cursor::guard_drain_progress(
                    source_index,
                    page_count,
                    &next,
                    &mut seen_inner_cursors,
                )?;
                inner_cursor = Some(next);
            }
            None => return Ok(false),
        }
    }
    unreachable!("the for loop's range has no upper bound")
}

/// Composes several [`CompletionProvider`]s into one completion capability, routed by
/// reference.
///
/// `complete` reads the [`CompleteRequestParams::r#ref`](CompleteRequestParams) field of
/// the incoming request:
///
/// - A [`Reference::Prompt`] routes to the first composed prompt source (in
///   [`MergedCompletionProvider::with_prompt_source`] call order) whose `list_prompts`
///   lists a prompt with that name.
/// - A [`Reference::Resource`] routes to the first composed resource source (in
///   [`MergedCompletionProvider::with_resource_source`] call order) whose
///   `list_resource_templates` lists a resource template with that URI.
///
/// A reference matching no composed source answers `CompleteResult::default()`: this
/// mirrors `rmcp`'s own default `complete` implementation (`handler/server.rs`, rmcp
/// 3.5.1), which answers the same default for every reference, so a merge type that
/// cannot resolve a reference matches the ecosystem convention rather than erroring.
///
/// # One slot, one value: the listing/completion correlation
///
/// Each composed source is one value that is both a listing provider
/// ([`PromptsProvider`] or [`ResourcesProvider`]) and a [`CompletionProvider`]. This is
/// a deliberate composition constraint, not an incidental limitation: routing decides
/// *which* composed value's `complete` to call by asking that *same* value whether it
/// lists the referenced name or URI. A caller that instead composes a prompt source's
/// listing from one value and its completion behavior from a different value (two
/// separate lists correlated only by matching index, as a naive
/// `Vec<Box<dyn PromptsProvider>>` plus a parallel `Vec<Box<dyn CompletionProvider>>`
/// would require) gets no such option here: [`MergedCompletionProvider::with_prompt_source`]
/// and [`MergedCompletionProvider::with_resource_source`] each take one value bounded by
/// both traits, so the listing a slot answers and the completion it serves are
/// structurally the same object, never a caller-maintained pairing that can drift.
pub struct MergedCompletionProvider {
    /// The composed prompt sources, each both a [`PromptsProvider`] and a
    /// [`CompletionProvider`], tried in `with_prompt_source` call order.
    prompt_sources: Vec<Box<dyn DynPromptCompletionSource>>,
    /// The composed resource sources, each both a [`ResourcesProvider`] and a
    /// [`CompletionProvider`], tried in `with_resource_source` call order.
    resource_sources: Vec<Box<dyn DynResourceCompletionSource>>,
}

impl MergedCompletionProvider {
    /// Starts an empty composition.
    ///
    /// Add sources with [`MergedCompletionProvider::with_prompt_source`] and
    /// [`MergedCompletionProvider::with_resource_source`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            prompt_sources: Vec::new(),
            resource_sources: Vec::new(),
        }
    }

    /// Adds `source` to the composition, as the routing target for a prompt-name
    /// reference it lists.
    ///
    /// `source` answers both the listing ([`PromptsProvider::list_prompts`]) that
    /// decides whether a prompt-name reference routes to it, and the completion
    /// ([`CompletionProvider::complete`]) that answers once routed. See
    /// [`MergedCompletionProvider`]'s doc comment for why these two capabilities come
    /// from the same value.
    #[must_use]
    pub fn with_prompt_source<T: PromptsProvider + CompletionProvider>(
        mut self,
        source: T,
    ) -> Self {
        self.prompt_sources.push(Box::new(source));
        self
    }

    /// Adds `source` to the composition, as the routing target for a
    /// resource-template-URI reference it lists.
    ///
    /// `source` answers both the listing
    /// ([`ResourcesProvider::list_resource_templates`]) that decides whether a
    /// resource-template reference routes to it, and the completion
    /// ([`CompletionProvider::complete`]) that answers once routed. See
    /// [`MergedCompletionProvider`]'s doc comment for why these two capabilities come
    /// from the same value.
    #[must_use]
    pub fn with_resource_source<T: ResourcesProvider + CompletionProvider>(
        mut self,
        source: T,
    ) -> Self {
        self.resource_sources.push(Box::new(source));
        self
    }
}

impl Default for MergedCompletionProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CompletionProvider for MergedCompletionProvider {
    async fn complete(
        &self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        match &request.r#ref {
            Reference::Prompt(prompt_reference) => {
                for (source_index, source) in self.prompt_sources.iter().enumerate() {
                    if source_lists_prompt_name(
                        source_index,
                        source.as_ref(),
                        &prompt_reference.name,
                        &context,
                    )
                    .await?
                    {
                        return source.complete(request, context).await;
                    }
                }
                Ok(CompleteResult::default())
            }
            Reference::Resource(resource_reference) => {
                for (source_index, source) in self.resource_sources.iter().enumerate() {
                    if source_lists_resource_template(
                        source_index,
                        source.as_ref(),
                        &resource_reference.uri,
                        &context,
                    )
                    .await?
                    {
                        return source.complete(request, context).await;
                    }
                }
                Ok(CompleteResult::default())
            }
            // `Reference` is `#[non_exhaustive]`: a reference variant this module does
            // not know about routes to no composed source, same as an unmatched known
            // variant.
            _ => Ok(CompleteResult::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use rmcp::model::{
        ArgumentInfo, CompletionInfo, GetPromptRequestParams, GetPromptResponse, ListPromptsResult,
        ListResourceTemplatesResult, Prompt, PromptMessage, ReadResourceRequestParams,
        ReadResourceResponse, Reference, ResourceTemplate, Role, SubscribeRequestParams,
        UnsubscribeRequestParams,
    };
    use rmcp::service::Peer;

    use super::*;

    /// A stub that is both a [`PromptsProvider`] and a [`CompletionProvider`], listing a
    /// fixed set of prompt names and answering a fixed, distinguishable
    /// [`CompleteResult`] marked with [`Stub::marker`].
    struct Stub {
        prompt_names: Vec<&'static str>,
        resource_templates: Vec<&'static str>,
        marker: &'static str,
    }

    impl Stub {
        fn with_prompt_names(marker: &'static str, prompt_names: Vec<&'static str>) -> Self {
            Self {
                prompt_names,
                resource_templates: Vec::new(),
                marker,
            }
        }

        fn with_resource_templates(
            marker: &'static str,
            resource_templates: Vec<&'static str>,
        ) -> Self {
            Self {
                prompt_names: Vec::new(),
                resource_templates,
                marker,
            }
        }

        /// Builds the marked [`CompleteResult`] this stub's `complete` answers: its
        /// single completion value is [`Stub::marker`], so a test can assert which
        /// stub answered by reading it back.
        fn marked_result(&self) -> CompleteResult {
            CompleteResult::new(
                CompletionInfo::new(vec![self.marker.to_string()])
                    .expect("one completion value is under CompletionInfo::MAX_VALUES"),
            )
        }
    }

    impl PromptsProvider for Stub {
        async fn list_prompts(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListPromptsResult, ErrorData> {
            Ok(ListPromptsResult::with_all_items(
                self.prompt_names
                    .iter()
                    .map(|name| Prompt::new(*name, None::<String>, None))
                    .collect(),
            ))
        }

        async fn get_prompt(
            &self,
            request: GetPromptRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<GetPromptResponse, ErrorData> {
            use rmcp::model::GetPromptResult;
            Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                Role::Assistant,
                format!("ran {name}", name = request.name),
            )])
            .into())
        }
    }

    impl ResourcesProvider for Stub {
        async fn list_resources(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<rmcp::model::ListResourcesResult, ErrorData> {
            Ok(rmcp::model::ListResourcesResult::with_all_items(Vec::new()))
        }

        async fn list_resource_templates(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListResourceTemplatesResult, ErrorData> {
            Ok(ListResourceTemplatesResult::with_all_items(
                self.resource_templates
                    .iter()
                    .map(|uri_template| ResourceTemplate::new(*uri_template, *uri_template))
                    .collect(),
            ))
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

    impl CompletionProvider for Stub {
        async fn complete(
            &self,
            _request: CompleteRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CompleteResult, ErrorData> {
            Ok(self.marked_result())
        }
    }

    fn prompt_request(name: &str) -> CompleteRequestParams {
        CompleteRequestParams::new(
            Reference::for_prompt(name),
            ArgumentInfo::new("argument", ""),
        )
    }

    fn resource_request(uri: &str) -> CompleteRequestParams {
        CompleteRequestParams::new(
            Reference::for_resource(uri),
            ArgumentInfo::new("argument", ""),
        )
    }

    /// A [`RequestContext<RoleServer>`] usable in a pure unit test.
    ///
    /// `rmcp`'s `Peer` has no public constructor outside a live client/server
    /// handshake, so this mints one over an in-memory duplex transport and then
    /// discards the connection: `MergedCompletionProvider` and `Stub` never read from
    /// `context.peer`, they only forward it, so a torn-down peer is a valid stand-in.
    async fn test_context() -> RequestContext<RoleServer> {
        use rmcp::ServiceExt;
        use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        let server = crate::ServerBuilder::new()
            .info(Implementation::new("merge-completion-test", "0.0.0"))
            .build();
        let server_task = tokio::spawn(async move {
            let running = server.serve(server_transport).await.expect("serve server");
            let peer: Peer<RoleServer> = running.peer().clone();
            running.waiting().await.expect("server stops");
            peer
        });

        let client_configuration = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("merge-completion-test-client", "0.0.0"),
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
    async fn prompt_reference_routes_to_the_source_that_lists_its_name() {
        let merged = MergedCompletionProvider::new()
            .with_prompt_source(Stub::with_prompt_names("a", vec!["a1"]))
            .with_prompt_source(Stub::with_prompt_names("b", vec!["b1"]));

        let result =
            CompletionProvider::complete(&merged, prompt_request("b1"), test_context().await)
                .await
                .expect("complete succeeds");

        assert_eq!(result.completion.values, vec!["b".to_string()]);
    }

    #[tokio::test]
    async fn resource_template_reference_routes_to_the_source_that_lists_its_uri() {
        let merged = MergedCompletionProvider::new()
            .with_resource_source(Stub::with_resource_templates("a", vec!["a://{id}"]))
            .with_resource_source(Stub::with_resource_templates("b", vec!["b://{id}"]));

        let result = CompletionProvider::complete(
            &merged,
            resource_request("b://{id}"),
            test_context().await,
        )
        .await
        .expect("complete succeeds");

        assert_eq!(result.completion.values, vec!["b".to_string()]);
    }

    /// A prompt source whose `list_prompts` always answers the same `next_cursor`,
    /// so a scan that does not guard against a repeated cursor never terminates.
    struct RepeatingPromptCursorStub;

    impl PromptsProvider for RepeatingPromptCursorStub {
        async fn list_prompts(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListPromptsResult, ErrorData> {
            let mut result =
                ListPromptsResult::with_all_items(vec![Prompt::new("a", None::<String>, None)]);
            result.next_cursor = Some("same-cursor".to_string());
            Ok(result)
        }

        async fn get_prompt(
            &self,
            _request: GetPromptRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<GetPromptResponse, ErrorData> {
            unimplemented!("not exercised by this test")
        }
    }

    impl CompletionProvider for RepeatingPromptCursorStub {
        async fn complete(
            &self,
            _request: CompleteRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CompleteResult, ErrorData> {
            unimplemented!("not exercised by this test: the scan never terminates")
        }
    }

    /// A resource source whose `list_resource_templates` always answers the same
    /// `next_cursor`, so a scan that does not guard against a repeated cursor never
    /// terminates.
    struct RepeatingTemplateCursorStub;

    impl ResourcesProvider for RepeatingTemplateCursorStub {
        async fn list_resources(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<rmcp::model::ListResourcesResult, ErrorData> {
            Ok(rmcp::model::ListResourcesResult::with_all_items(Vec::new()))
        }

        async fn list_resource_templates(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListResourceTemplatesResult, ErrorData> {
            let mut result =
                ListResourceTemplatesResult::with_all_items(vec![ResourceTemplate::new(
                    "a://{id}", "a://{id}",
                )]);
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

    impl CompletionProvider for RepeatingTemplateCursorStub {
        async fn complete(
            &self,
            _request: CompleteRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CompleteResult, ErrorData> {
            unimplemented!("not exercised by this test: the scan never terminates")
        }
    }

    #[tokio::test]
    async fn a_prompt_source_whose_next_cursor_always_repeats_is_refused_and_does_not_hang() {
        let merged = MergedCompletionProvider::new().with_prompt_source(RepeatingPromptCursorStub);

        let error =
            CompletionProvider::complete(&merged, prompt_request("missing"), test_context().await)
                .await
                .expect_err("a repeating cursor ends the scan instead of hanging");

        assert!(error.message.contains("provider 0"), "{error:?}");
    }

    #[tokio::test]
    async fn a_resource_source_whose_next_cursor_always_repeats_is_refused_and_does_not_hang() {
        let merged =
            MergedCompletionProvider::new().with_resource_source(RepeatingTemplateCursorStub);

        let error = CompletionProvider::complete(
            &merged,
            resource_request("missing://{id}"),
            test_context().await,
        )
        .await
        .expect_err("a repeating cursor ends the scan instead of hanging");

        assert!(error.message.contains("provider 0"), "{error:?}");
    }

    #[tokio::test]
    async fn prompt_reference_matching_no_source_answers_the_default_result() {
        let merged = MergedCompletionProvider::new()
            .with_prompt_source(Stub::with_prompt_names("a", vec!["a1"]));

        let result =
            CompletionProvider::complete(&merged, prompt_request("missing"), test_context().await)
                .await
                .expect("complete succeeds");

        assert_eq!(result, CompleteResult::default());
    }

    #[tokio::test]
    async fn resource_template_reference_matching_no_source_answers_the_default_result() {
        let merged = MergedCompletionProvider::new()
            .with_resource_source(Stub::with_resource_templates("a", vec!["a://{id}"]));

        let result = CompletionProvider::complete(
            &merged,
            resource_request("missing://{id}"),
            test_context().await,
        )
        .await
        .expect("complete succeeds");

        assert_eq!(result, CompleteResult::default());
    }
}
