//! Merges several [`PromptsProvider`]s into one prompts capability.
//!
//! [`MergedPromptsProvider`] concatenates every composed provider's prompt list, in
//! construction order, and routes a `get_prompt` by name to whichever provider lists it.
//! A prompt name listed by more than one provider is a composition error, not a silent
//! pick of one provider over the other.

use std::collections::HashMap;
use std::pin::Pin;

use rmcp::{
    model::{
        ErrorCode, ErrorData, GetPromptRequestParams, GetPromptResponse, ListPromptsResult,
        PaginatedRequestParams,
    },
    service::{RequestContext, RoleServer},
};

use crate::providers::PromptsProvider;

use super::listing;

/// Object-safe adapter over [`PromptsProvider`], boxing its futures.
///
/// `PromptsProvider`'s methods are written as `-> impl Future<Output = ...> + Send`
/// (return-position impl trait), which is not object-safe: `Box<dyn PromptsProvider>`
/// cannot be named. This sealed trait is implemented for every `T: PromptsProvider`
/// through a blanket implementation below, boxing each future so
/// [`MergedPromptsProvider`] can store `Vec<Box<dyn DynPromptsProvider>>` internally,
/// without changing `PromptsProvider`'s public, unboxed shape.
trait DynPromptsProvider: Send + Sync {
    /// Object-safe counterpart of [`PromptsProvider::list_prompts`].
    fn list_prompts<'provider>(
        &'provider self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<ListPromptsResult, ErrorData>> + Send + 'provider>>;

    /// Object-safe counterpart of [`PromptsProvider::get_prompt`].
    fn get_prompt<'provider>(
        &'provider self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<GetPromptResponse, ErrorData>> + Send + 'provider>>;
}

impl<T: PromptsProvider> DynPromptsProvider for T {
    fn list_prompts<'provider>(
        &'provider self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<ListPromptsResult, ErrorData>> + Send + 'provider>>
    {
        Box::pin(PromptsProvider::list_prompts(self, request, context))
    }

    fn get_prompt<'provider>(
        &'provider self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<GetPromptResponse, ErrorData>> + Send + 'provider>>
    {
        Box::pin(PromptsProvider::get_prompt(self, request, context))
    }
}

/// Composes several [`PromptsProvider`]s into one prompts capability.
///
/// `list_prompts` concatenates every inner provider's prompts, in construction order.
/// `get_prompt` routes a call by name to the provider that lists it.
///
/// # Duplicate prompt names
///
/// A prompt name listed by more than one inner provider is a composition error: both
/// [`list_prompts`](PromptsProvider::list_prompts) and
/// [`get_prompt`](PromptsProvider::get_prompt) answer `Err(ErrorData::invalid_params(...))`
/// naming the prompt and the two providers, by their 0-indexed construction order, rather
/// than silently preferring one provider over the other.
pub struct MergedPromptsProvider {
    /// The composed providers, in listing and routing order.
    providers: Vec<Box<dyn DynPromptsProvider>>,
}

impl MergedPromptsProvider {
    /// Starts an empty composition.
    ///
    /// Add providers with [`MergedPromptsProvider::with_provider`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Adds `provider` to the composition, after every provider added so far.
    ///
    /// A provider's 0-indexed position among `with_provider` calls is the index
    /// [`MergedPromptsProvider`] uses in a duplicate-prompt-name error and in the merge
    /// cursor it hands out.
    #[must_use]
    pub fn with_provider<T: PromptsProvider>(mut self, provider: T) -> Self {
        self.providers.push(Box::new(provider));
        self
    }

    /// Drains every composed provider's full prompt list, following each provider's
    /// own pagination until it answers `next_cursor: None`, via the shared
    /// [`listing::drain`] skeleton.
    ///
    /// Returns each prompt tagged with the 0-indexed provider that listed it, in
    /// provider-then-page order.
    async fn drain_prompts(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Vec<(usize, rmcp::model::Prompt)>, ErrorData> {
        listing::drain(self.providers.len(), |provider_index, inner_cursor| {
            let provider = &self.providers[provider_index];
            let context = context.clone();
            Box::pin(async move {
                let result = provider
                    .list_prompts(
                        Some(PaginatedRequestParams::default().with_cursor(inner_cursor)),
                        context,
                    )
                    .await?;
                Ok(listing::Page {
                    items: result.prompts,
                    next_cursor: result.next_cursor,
                })
            })
        })
        .await
    }

    /// Maps every composed provider's prompt names to the provider that listed them,
    /// via the shared [`listing::index_by_provider`] skeleton.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-key error (kind `"prompt"`) for the first prompt name
    /// found listed by two different providers (by 0-indexed construction order).
    async fn index_prompt_names_by_provider(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<HashMap<String, usize>, ErrorData> {
        let drained = self.drain_prompts(context).await?;
        listing::index_by_provider(
            drained,
            "prompt",
            |prompt| prompt.name.clone(),
            |name| name.clone(),
        )
    }
}

impl Default for MergedPromptsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptsProvider for MergedPromptsProvider {
    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let merge_cursor = request.as_ref().and_then(|params| params.cursor.as_deref());

        // Every call re-validates the whole composition: a duplicate introduced by a
        // provider whose own prompt list changed between calls must surface here too.
        // Skipped for an empty composition: there is nothing to list or to duplicate.
        if !self.providers.is_empty() {
            self.index_prompt_names_by_provider(&context).await?;
        }

        let (prompts, next_cursor) = listing::list_one_page(
            merge_cursor,
            self.providers.len(),
            |provider_index, inner_cursor| {
                let provider = &self.providers[provider_index];
                let context = context.clone();
                Box::pin(async move {
                    let result = provider
                        .list_prompts(
                            Some(PaginatedRequestParams::default().with_cursor(inner_cursor)),
                            context,
                        )
                        .await?;
                    Ok(listing::Page {
                        items: result.prompts,
                        next_cursor: result.next_cursor,
                    })
                })
            },
        )
        .await?;

        let mut result = ListPromptsResult::with_all_items(prompts);
        result.next_cursor = next_cursor;
        Ok(result)
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let owner_by_name = self.index_prompt_names_by_provider(&context).await?;

        let provider_index = *owner_by_name.get(&request.name).ok_or_else(|| {
            ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                format!(
                    "no composed provider lists prompt {name:?}",
                    name = request.name
                ),
                None,
            )
        })?;

        let provider = self
            .providers
            .get(provider_index)
            .expect("index_prompt_names_by_provider only returns in-range provider indices");

        provider.get_prompt(request, context).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rmcp::model::{GetPromptResult, Prompt, PromptMessage, Role};
    use rmcp::service::Peer;

    use super::super::cursor;
    use super::*;

    /// An in-memory [`PromptsProvider`] stub serving fixed pages, by cursor, in order.
    ///
    /// `pages` is consumed front-to-back: the first page answers a `None` cursor, and
    /// each page's own `Option<String>` cursor value is the one the *next* `list_prompts`
    /// call must present for `Stub` to serve the following page. `get_prompt` answers
    /// its own `label` tagged together with the requested name, so a test can assert
    /// which stub a routed call reached, not merely that some stub answered.
    struct Stub {
        label: &'static str,
        pages: Mutex<Vec<(Vec<Prompt>, Option<String>)>>,
    }

    impl Stub {
        fn new(label: &'static str, pages: Vec<(Vec<Prompt>, Option<String>)>) -> Self {
            Self {
                label,
                pages: Mutex::new(pages),
            }
        }

        /// A stub with every prompt on a single page (`next_cursor: None`).
        fn single_page(label: &'static str, prompts: Vec<Prompt>) -> Self {
            Self::new(label, vec![(prompts, None)])
        }
    }

    impl PromptsProvider for Stub {
        async fn list_prompts(
            &self,
            request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListPromptsResult, ErrorData> {
            let requested_cursor = request.and_then(|params| params.cursor);
            let pages = self.pages.lock().expect("stub pages lock");
            let page_index = match &requested_cursor {
                None => 0,
                Some(cursor) => pages
                    .iter()
                    .position(|(_, next_cursor)| next_cursor.as_deref() == Some(cursor.as_str()))
                    .map(|index| index + 1)
                    .expect("test passes back a cursor this stub produced"),
            };
            let (prompts, next_cursor) = pages
                .get(page_index)
                .cloned()
                .expect("test does not request a page past this stub's last one");
            let mut result = ListPromptsResult::with_all_items(prompts);
            result.next_cursor = next_cursor;
            Ok(result)
        }

        async fn get_prompt(
            &self,
            request: GetPromptRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<GetPromptResponse, ErrorData> {
            Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                Role::Assistant,
                format!(
                    "provider {label} ran {name}",
                    label = self.label,
                    name = request.name
                ),
            )])
            .into())
        }
    }

    fn prompt(name: &'static str) -> Prompt {
        Prompt::new(name, None::<String>, None)
    }

    /// A [`RequestContext<RoleServer>`] usable in a pure unit test.
    ///
    /// `rmcp`'s `Peer` has no public constructor outside a live client/server
    /// handshake, so this mints one over an in-memory duplex transport and then
    /// discards the connection: `MergedPromptsProvider` and `Stub` never read from
    /// `context.peer`, they only forward it, so a torn-down peer is a valid stand-in.
    async fn test_context() -> RequestContext<RoleServer> {
        use rmcp::ServiceExt;
        use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        let server = crate::ServerBuilder::new()
            .info(Implementation::new("merge-prompts-test", "0.0.0"))
            .build();
        let server_task = tokio::spawn(async move {
            let running = server.serve(server_transport).await.expect("serve server");
            let peer: Peer<RoleServer> = running.peer().clone();
            running.waiting().await.expect("server stops");
            peer
        });

        let client_configuration = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("merge-prompts-test-client", "0.0.0"),
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
    async fn an_empty_composition_lists_no_prompts_and_no_next_cursor() {
        let merged = MergedPromptsProvider::new();

        let result = PromptsProvider::list_prompts(&merged, None, test_context().await)
            .await
            .expect("an empty composition lists nothing instead of erroring");

        assert!(result.prompts.is_empty());
        assert_eq!(result.next_cursor, None);
    }

    #[tokio::test]
    async fn a_cursor_naming_a_missing_provider_on_an_empty_composition_is_still_an_error() {
        let merged = MergedPromptsProvider::new();
        let request =
            Some(PaginatedRequestParams::default().with_cursor(Some(cursor::encode(0, None))));

        let error = PromptsProvider::list_prompts(&merged, request, test_context().await)
            .await
            .expect_err("a supplied cursor naming a missing provider stays an error");

        assert!(error.message.contains('0'), "{error:?}");
    }

    #[tokio::test]
    async fn concatenates_non_overlapping_single_page_providers() {
        // `MergedPromptsProvider::list_prompts` answers exactly one inner provider's page
        // per call (see its doc comment), so a client that pages from `None` through
        // every `next_cursor` sees all 4 prompts, concatenated in provider order, across
        // the two merge-level pages this composition produces (one per provider).
        let merged = MergedPromptsProvider::new()
            .with_provider(Stub::single_page("A", vec![prompt("a1"), prompt("a2")]))
            .with_provider(Stub::single_page("B", vec![prompt("b1"), prompt("b2")]));

        let mut all_prompt_names = Vec::new();
        let mut cursor = None;
        loop {
            let request = cursor
                .take()
                .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
            let result = PromptsProvider::list_prompts(&merged, request, test_context().await)
                .await
                .expect("list_prompts succeeds");
            all_prompt_names.extend(result.prompts.iter().map(|prompt| prompt.name.clone()));
            match result.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        assert_eq!(all_prompt_names, vec!["a1", "a2", "b1", "b2"]);
    }

    #[tokio::test]
    async fn paginates_across_two_providers_with_two_pages_each() {
        let merged = MergedPromptsProvider::new()
            .with_provider(Stub::new(
                "A",
                vec![
                    (vec![prompt("a1")], Some("a-page-2".to_string())),
                    (vec![prompt("a2")], None),
                ],
            ))
            .with_provider(Stub::new(
                "B",
                vec![
                    (vec![prompt("b1")], Some("b-page-2".to_string())),
                    (vec![prompt("b2")], None),
                ],
            ));

        let mut seen_prompt_names = Vec::new();
        let mut cursor = None;
        for _ in 0..4 {
            let context = test_context().await;
            let request = Some(PaginatedRequestParams::default().with_cursor(cursor.clone()));
            let result = PromptsProvider::list_prompts(&merged, request, context)
                .await
                .expect("list_prompts succeeds");
            assert_eq!(result.prompts.len(), 1, "one provider-page per merge page");
            seen_prompt_names.push(result.prompts[0].name.clone());
            cursor = result.next_cursor;
        }

        assert_eq!(seen_prompt_names, vec!["a1", "a2", "b1", "b2"]);
        assert_eq!(cursor, None, "the fourth call ends pagination");
    }

    /// A [`PromptsProvider`] whose `list_prompts` always answers the same
    /// `next_cursor`, so a drain that does not guard against a repeated cursor never
    /// terminates.
    struct RepeatingCursorStub;

    impl PromptsProvider for RepeatingCursorStub {
        async fn list_prompts(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListPromptsResult, ErrorData> {
            let mut result = ListPromptsResult::with_all_items(vec![prompt("a")]);
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

    #[tokio::test]
    async fn a_provider_whose_next_cursor_always_repeats_is_refused_and_does_not_hang() {
        let merged = MergedPromptsProvider::new().with_provider(RepeatingCursorStub);

        let error = PromptsProvider::list_prompts(&merged, None, test_context().await)
            .await
            .expect_err("a repeating cursor ends the drain instead of hanging");

        assert!(error.message.contains("provider 0"), "{error:?}");
    }

    #[tokio::test]
    async fn rejects_a_prompt_name_shared_by_two_providers() {
        let merged = MergedPromptsProvider::new()
            .with_provider(Stub::single_page("A", vec![prompt("shared")]))
            .with_provider(Stub::single_page("B", vec![prompt("shared")]));

        let error = PromptsProvider::list_prompts(&merged, None, test_context().await)
            .await
            .expect_err("list_prompts reports the duplicate");

        assert!(error.message.contains("shared"), "{error:?}");
    }

    #[tokio::test]
    async fn routes_get_prompt_to_the_provider_that_lists_the_name() {
        let merged = MergedPromptsProvider::new()
            .with_provider(Stub::single_page("A", vec![prompt("a1")]))
            .with_provider(Stub::single_page("B", vec![prompt("b1")]));

        let response = PromptsProvider::get_prompt(
            &merged,
            GetPromptRequestParams::new("b1"),
            test_context().await,
        )
        .await
        .expect("get_prompt succeeds");

        let GetPromptResponse::Complete(result) = response else {
            panic!("expected a complete result");
        };
        let text = result.messages[0].content.as_text().expect("text content");
        assert_eq!(text.text, "provider B ran b1");

        let response = PromptsProvider::get_prompt(
            &merged,
            GetPromptRequestParams::new("a1"),
            test_context().await,
        )
        .await
        .expect("get_prompt succeeds");

        let GetPromptResponse::Complete(result) = response else {
            panic!("expected a complete result");
        };
        let text = result.messages[0].content.as_text().expect("text content");
        assert_eq!(text.text, "provider A ran a1");
    }

    #[tokio::test]
    async fn rejects_a_call_to_an_unknown_prompt_name() {
        let merged =
            MergedPromptsProvider::new().with_provider(Stub::single_page("A", vec![prompt("a1")]));

        let error = PromptsProvider::get_prompt(
            &merged,
            GetPromptRequestParams::new("missing"),
            test_context().await,
        )
        .await
        .expect_err("get_prompt reports the unknown name");

        assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn rejects_a_call_to_a_name_shared_by_two_providers() {
        let merged = MergedPromptsProvider::new()
            .with_provider(Stub::single_page("A", vec![prompt("shared")]))
            .with_provider(Stub::single_page("B", vec![prompt("shared")]));

        let error = PromptsProvider::get_prompt(
            &merged,
            GetPromptRequestParams::new("shared"),
            test_context().await,
        )
        .await
        .expect_err("get_prompt reports the duplicate");

        assert!(error.message.contains("shared"), "{error:?}");
    }
}
