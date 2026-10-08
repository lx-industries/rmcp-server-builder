//! Merges several [`ToolsProvider`]s into one tools capability.
//!
//! [`MergedToolsProvider`] concatenates every composed provider's tool list, in
//! construction order, and routes a `call_tool` by name to whichever provider lists it.
//! A tool name listed by more than one provider is a composition error, not a silent
//! pick of one provider over the other.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "consumed once src/lib.rs re-exports MergedToolsProvider (task 9); \
                  exercised directly by this module's own tests until then"
    )
)]

use std::collections::HashMap;
use std::pin::Pin;

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, ErrorCode, ErrorData, ListToolsResult,
        PaginatedRequestParams,
    },
    service::{RequestContext, RoleServer},
};

use crate::providers::ToolsProvider;

use super::cursor;

/// Object-safe adapter over [`ToolsProvider`], boxing its futures.
///
/// `ToolsProvider`'s methods are written as `-> impl Future<Output = ...> + Send`
/// (return-position impl trait), which is not object-safe: `Box<dyn ToolsProvider>`
/// cannot be named. This sealed trait is implemented for every `T: ToolsProvider`
/// through a blanket implementation below, boxing each future so
/// [`MergedToolsProvider`] can store `Vec<Box<dyn DynToolsProvider>>` internally,
/// without changing `ToolsProvider`'s public, unboxed shape.
trait DynToolsProvider: Send + Sync {
    /// Object-safe counterpart of [`ToolsProvider::list_tools`].
    fn list_tools<'provider>(
        &'provider self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<ListToolsResult, ErrorData>> + Send + 'provider>>;

    /// Object-safe counterpart of [`ToolsProvider::call_tool`].
    fn call_tool<'provider>(
        &'provider self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<CallToolResponse, ErrorData>> + Send + 'provider>>;
}

impl<T: ToolsProvider> DynToolsProvider for T {
    fn list_tools<'provider>(
        &'provider self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<ListToolsResult, ErrorData>> + Send + 'provider>> {
        Box::pin(ToolsProvider::list_tools(self, request, context))
    }

    fn call_tool<'provider>(
        &'provider self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<CallToolResponse, ErrorData>> + Send + 'provider>> {
        Box::pin(ToolsProvider::call_tool(self, request, context))
    }
}

/// Builds the duplicate-tool-name error [`MergedToolsProvider`] answers when
/// `tool_name` is listed by both `first_provider_index` and `second_provider_index`.
fn duplicate_tool_name_error(
    tool_name: &str,
    first_provider_index: usize,
    second_provider_index: usize,
) -> ErrorData {
    ErrorData::invalid_params(
        format!(
            "tool {tool_name:?} is listed by both provider {first_provider_index} and \
             provider {second_provider_index}"
        ),
        None,
    )
}

/// Composes several [`ToolsProvider`]s into one tools capability.
///
/// `list_tools` concatenates every inner provider's tools, in construction order.
/// `call_tool` routes a call by name to the provider that lists it.
///
/// # Duplicate tool names
///
/// A tool name listed by more than one inner provider is a composition error: both
/// [`list_tools`](ToolsProvider::list_tools) and [`call_tool`](ToolsProvider::call_tool)
/// answer `Err(ErrorData::invalid_params(...))` naming the tool and the two providers,
/// by their 0-indexed construction order, rather than silently preferring one provider
/// over the other.
pub struct MergedToolsProvider {
    /// The composed providers, in listing and routing order.
    providers: Vec<Box<dyn DynToolsProvider>>,
}

impl MergedToolsProvider {
    /// Starts an empty composition.
    ///
    /// Add providers with [`MergedToolsProvider::with_provider`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Adds `provider` to the composition, after every provider added so far.
    ///
    /// A provider's 0-indexed position among `with_provider` calls is the index
    /// [`MergedToolsProvider`] uses in a duplicate-tool-name error and in the merge
    /// cursor it hands out.
    #[must_use]
    pub fn with_provider<T: ToolsProvider>(mut self, provider: T) -> Self {
        self.providers.push(Box::new(provider));
        self
    }

    /// Drains every composed provider's full tool list, following each provider's own
    /// pagination until it answers `next_cursor: None`.
    ///
    /// Returns each tool's name tagged with the 0-indexed provider that listed it, in
    /// provider-then-page order.
    async fn drain_tool_names(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Vec<(usize, String)>, ErrorData> {
        let mut named = Vec::new();
        for (provider_index, provider) in self.providers.iter().enumerate() {
            let mut inner_cursor = None;
            let mut seen_inner_cursors = std::collections::HashSet::new();
            for page_count in 0.. {
                let page = provider
                    .list_tools(
                        Some(PaginatedRequestParams::default().with_cursor(inner_cursor)),
                        context.clone(),
                    )
                    .await?;
                named.extend(
                    page.tools
                        .iter()
                        .map(|tool| (provider_index, tool.name.to_string())),
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
        Ok(named)
    }

    /// Maps every composed provider's tool names to the provider that listed them.
    ///
    /// # Errors
    ///
    /// Returns [`duplicate_tool_name_error`] for the first tool name found listed by
    /// two different providers (by 0-indexed construction order).
    async fn index_tool_names_by_provider(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<HashMap<String, usize>, ErrorData> {
        let named = self.drain_tool_names(context).await?;
        let mut owner_by_name = HashMap::new();
        for (provider_index, tool_name) in named {
            match owner_by_name.get(&tool_name) {
                Some(&owning_index) if owning_index != provider_index => {
                    return Err(duplicate_tool_name_error(
                        &tool_name,
                        owning_index,
                        provider_index,
                    ));
                }
                _ => {
                    owner_by_name.insert(tool_name, provider_index);
                }
            }
        }
        Ok(owner_by_name)
    }
}

impl Default for MergedToolsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolsProvider for MergedToolsProvider {
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let cursor = request.as_ref().and_then(|params| params.cursor.as_deref());
        if cursor.is_none() && self.providers.is_empty() {
            // An empty composition has no provider to page into: answer an empty page
            // directly rather than treating provider index 0 as in range.
            return Ok(ListToolsResult::with_all_items(Vec::new()));
        }
        let (provider_index, inner_cursor) = match cursor {
            Some(cursor) => cursor::decode(cursor)?,
            None => (0, None),
        };

        // Every call re-validates the whole composition: a duplicate introduced by a
        // provider whose own tool list changed between calls must surface here too.
        self.index_tool_names_by_provider(&context).await?;

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
            .list_tools(
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

        let mut result = ListToolsResult::with_all_items(page.tools);
        result.next_cursor = next_cursor;
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let owner_by_name = self.index_tool_names_by_provider(&context).await?;

        let provider_index = *owner_by_name.get(request.name.as_ref()).ok_or_else(|| {
            ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                format!(
                    "no composed provider lists tool {name:?}",
                    name = request.name
                ),
                None,
            )
        })?;

        let provider = self
            .providers
            .get(provider_index)
            .expect("index_tool_names_by_provider only returns in-range provider indices");

        provider.call_tool(request, context).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rmcp::model::{CallToolResult, ContentBlock, RequestId, Tool};
    use rmcp::service::Peer;

    use super::*;

    /// An in-memory [`ToolsProvider`] stub serving fixed pages, by cursor, in order.
    ///
    /// `pages` is consumed front-to-back: the first page answers a `None` cursor, and
    /// each page's own `Option<String>` cursor value is the one the *next* `list_tools`
    /// call must present for `Stub` to serve the following page.
    struct Stub {
        pages: Mutex<Vec<(Vec<Tool>, Option<String>)>>,
    }

    impl Stub {
        fn new(pages: Vec<(Vec<Tool>, Option<String>)>) -> Self {
            Self {
                pages: Mutex::new(pages),
            }
        }

        /// A stub with every tool on a single page (`next_cursor: None`).
        fn single_page(tools: Vec<Tool>) -> Self {
            Self::new(vec![(tools, None)])
        }
    }

    impl ToolsProvider for Stub {
        async fn list_tools(
            &self,
            request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
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
            let (tools, next_cursor) = pages
                .get(page_index)
                .cloned()
                .expect("test does not request a page past this stub's last one");
            let mut result = ListToolsResult::with_all_items(tools);
            result.next_cursor = next_cursor;
            Ok(result)
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "ran {name}",
                name = request.name
            ))])
            .into())
        }
    }

    fn tool(name: &'static str) -> Tool {
        Tool::new(name, "a test tool", rmcp::model::JsonObject::new())
    }

    /// A [`RequestContext<RoleServer>`] usable in a pure unit test.
    ///
    /// `rmcp`'s `Peer` has no public constructor outside a live client/server
    /// handshake, so this mints one over an in-memory duplex transport and then
    /// discards the connection: `MergedToolsProvider` and `Stub` never read from
    /// `context.peer`, they only forward it, so a torn-down peer is a valid stand-in.
    async fn test_context() -> RequestContext<RoleServer> {
        use rmcp::ServiceExt;
        use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};

        let (server_transport, client_transport) = tokio::io::duplex(4096);
        let server = crate::ServerBuilder::new()
            .info(Implementation::new("merge-tools-test", "0.0.0"))
            .build();
        let server_task = tokio::spawn(async move {
            let running = server.serve(server_transport).await.expect("serve server");
            let peer: Peer<RoleServer> = running.peer().clone();
            running.waiting().await.expect("server stops");
            peer
        });

        let client_configuration = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("merge-tools-test-client", "0.0.0"),
        );
        let client = client_configuration
            .serve(client_transport)
            .await
            .expect("initialize client");
        client.cancel().await.expect("cancel client");
        let peer = server_task.await.expect("server task");

        RequestContext::new(RequestId::Number(0), peer)
    }

    #[tokio::test]
    async fn an_empty_composition_lists_no_tools_and_no_next_cursor() {
        let merged = MergedToolsProvider::new();

        let result = ToolsProvider::list_tools(&merged, None, test_context().await)
            .await
            .expect("an empty composition lists nothing instead of erroring");

        assert!(result.tools.is_empty());
        assert_eq!(result.next_cursor, None);
    }

    #[tokio::test]
    async fn a_cursor_naming_a_missing_provider_on_an_empty_composition_is_still_an_error() {
        let merged = MergedToolsProvider::new();
        let request =
            Some(PaginatedRequestParams::default().with_cursor(Some(cursor::encode(0, None))));

        let error = ToolsProvider::list_tools(&merged, request, test_context().await)
            .await
            .expect_err("a supplied cursor naming a missing provider stays an error");

        assert!(error.message.contains('0'), "{error:?}");
    }

    #[tokio::test]
    async fn concatenates_non_overlapping_single_page_providers() {
        // `MergedToolsProvider::list_tools` answers exactly one inner provider's page
        // per call (see its doc comment), so a client that pages from `None` through
        // every `next_cursor` sees all 4 tools, concatenated in provider order, across
        // the two merge-level pages this composition produces (one per provider).
        let merged = MergedToolsProvider::new()
            .with_provider(Stub::single_page(vec![tool("a1"), tool("a2")]))
            .with_provider(Stub::single_page(vec![tool("b1"), tool("b2")]));

        let mut all_tool_names = Vec::new();
        let mut cursor = None;
        loop {
            let request = cursor
                .take()
                .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
            let result = ToolsProvider::list_tools(&merged, request, test_context().await)
                .await
                .expect("list_tools succeeds");
            all_tool_names.extend(result.tools.iter().map(|tool| tool.name.to_string()));
            match result.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        assert_eq!(all_tool_names, vec!["a1", "a2", "b1", "b2"]);
    }

    #[tokio::test]
    async fn paginates_across_two_providers_with_two_pages_each() {
        let merged = MergedToolsProvider::new()
            .with_provider(Stub::new(vec![
                (vec![tool("a1")], Some("a-page-2".to_string())),
                (vec![tool("a2")], None),
            ]))
            .with_provider(Stub::new(vec![
                (vec![tool("b1")], Some("b-page-2".to_string())),
                (vec![tool("b2")], None),
            ]));

        let mut seen_tool_names = Vec::new();
        let mut cursor = None;
        for _ in 0..4 {
            let context = test_context().await;
            let request = Some(PaginatedRequestParams::default().with_cursor(cursor.clone()));
            let result = ToolsProvider::list_tools(&merged, request, context)
                .await
                .expect("list_tools succeeds");
            assert_eq!(result.tools.len(), 1, "one provider-page per merge page");
            seen_tool_names.push(result.tools[0].name.to_string());
            cursor = result.next_cursor;
        }

        assert_eq!(seen_tool_names, vec!["a1", "a2", "b1", "b2"]);
        assert_eq!(cursor, None, "the fourth call ends pagination");
    }

    /// A [`ToolsProvider`] whose `list_tools` always answers the same `next_cursor`,
    /// so a drain that does not guard against a repeated cursor never terminates.
    struct RepeatingCursorStub;

    impl ToolsProvider for RepeatingCursorStub {
        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            let mut result = ListToolsResult::with_all_items(vec![tool("a")]);
            result.next_cursor = Some("same-cursor".to_string());
            Ok(result)
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            unimplemented!("not exercised by this test")
        }
    }

    #[tokio::test]
    async fn a_provider_whose_next_cursor_always_repeats_is_refused_and_does_not_hang() {
        let merged = MergedToolsProvider::new().with_provider(RepeatingCursorStub);

        let error = ToolsProvider::list_tools(&merged, None, test_context().await)
            .await
            .expect_err("a repeating cursor ends the drain instead of hanging");

        assert!(error.message.contains("provider 0"), "{error:?}");
    }

    #[tokio::test]
    async fn rejects_a_tool_name_shared_by_two_providers() {
        let merged = MergedToolsProvider::new()
            .with_provider(Stub::single_page(vec![tool("shared")]))
            .with_provider(Stub::single_page(vec![tool("shared")]));

        let error = ToolsProvider::list_tools(&merged, None, test_context().await)
            .await
            .expect_err("list_tools reports the duplicate");

        assert!(error.message.contains("shared"), "{error:?}");
    }

    #[tokio::test]
    async fn routes_call_tool_to_the_provider_that_lists_the_name() {
        let merged = MergedToolsProvider::new()
            .with_provider(Stub::single_page(vec![tool("a1")]))
            .with_provider(Stub::single_page(vec![tool("b1")]));

        let response = ToolsProvider::call_tool(
            &merged,
            CallToolRequestParams::new("b1"),
            test_context().await,
        )
        .await
        .expect("call_tool succeeds");

        let CallToolResponse::Complete(result) = response else {
            panic!("expected a complete result");
        };
        let text = result.content[0].as_text().expect("text content");
        assert_eq!(text.text, "ran b1");
    }

    #[tokio::test]
    async fn rejects_a_call_to_an_unknown_tool_name() {
        let merged = MergedToolsProvider::new().with_provider(Stub::single_page(vec![tool("a1")]));

        let error = ToolsProvider::call_tool(
            &merged,
            CallToolRequestParams::new("missing"),
            test_context().await,
        )
        .await
        .expect_err("call_tool reports the unknown name");

        assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn rejects_a_call_to_a_name_shared_by_two_providers() {
        let merged = MergedToolsProvider::new()
            .with_provider(Stub::single_page(vec![tool("shared")]))
            .with_provider(Stub::single_page(vec![tool("shared")]));

        let error = ToolsProvider::call_tool(
            &merged,
            CallToolRequestParams::new("shared"),
            test_context().await,
        )
        .await
        .expect_err("call_tool reports the duplicate");

        assert!(error.message.contains("shared"), "{error:?}");
    }
}
