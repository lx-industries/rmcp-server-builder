//! Merges several [`ToolsProvider`]s into one tools capability.
//!
//! [`MergedToolsProvider`] concatenates every composed provider's tool list, in
//! construction order, and routes a `call_tool` by name to whichever provider lists it.
//! A tool name listed by more than one provider is a composition error, not a silent
//! pick of one provider over the other.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Mutex;

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, ErrorCode, ErrorData,
        GetTaskParams, GetTaskResult, ListToolsResult, PaginatedRequestParams, UpdateTaskParams,
    },
    service::{RequestContext, RoleServer},
};

use crate::providers::ToolsProvider;

use super::listing;

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

    /// Object-safe counterpart of [`ToolsProvider::get_task`].
    fn get_task<'provider>(
        &'provider self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<GetTaskResult, ErrorData>> + Send + 'provider>>;

    /// Object-safe counterpart of [`ToolsProvider::update_task`].
    fn update_task<'provider>(
        &'provider self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>>;

    /// Object-safe counterpart of [`ToolsProvider::cancel_task`].
    fn cancel_task<'provider>(
        &'provider self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>>;
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

    fn get_task<'provider>(
        &'provider self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<GetTaskResult, ErrorData>> + Send + 'provider>> {
        Box::pin(ToolsProvider::get_task(self, request, context))
    }

    fn update_task<'provider>(
        &'provider self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>> {
        Box::pin(ToolsProvider::update_task(self, request, context))
    }

    fn cancel_task<'provider>(
        &'provider self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>> {
        Box::pin(ToolsProvider::cancel_task(self, request, context))
    }
}

/// Builds the task-id-collision error [`MergedToolsProvider::call_tool`] answers when
/// `task_id` is already recorded for `existing_provider_index`, and a different
/// provider, `new_provider_index`, just answered a task with the same id.
fn task_id_collision_error(
    task_id: &str,
    existing_provider_index: usize,
    new_provider_index: usize,
) -> ErrorData {
    ErrorData::invalid_params(
        format!(
            "task id {task_id:?} is already owned by provider {existing_provider_index}, \
             but provider {new_provider_index} just created a task with the same id"
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
    /// The 0-indexed composed provider that created each still-tracked task, by task id.
    ///
    /// `call_tool` records an entry here the moment a provider answers with
    /// [`CallToolResponse::Task`]; `get_task`, `update_task` and `cancel_task` read it to
    /// route a `tasks/*` call to the provider that created the task.
    provider_index_by_task_id: Mutex<HashMap<String, usize>>,
}

impl MergedToolsProvider {
    /// Starts an empty composition.
    ///
    /// Add providers with [`MergedToolsProvider::with_provider`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            provider_index_by_task_id: Mutex::new(HashMap::new()),
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
    /// pagination until it answers `next_cursor: None`, via the shared
    /// [`listing::drain`] skeleton.
    ///
    /// Returns each tool's name tagged with the 0-indexed provider that listed it, in
    /// provider-then-page order.
    async fn drain_tools(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Vec<(usize, rmcp::model::Tool)>, ErrorData> {
        listing::drain(self.providers.len(), |provider_index, inner_cursor| {
            let provider = &self.providers[provider_index];
            let context = context.clone();
            Box::pin(async move {
                let result = provider
                    .list_tools(
                        Some(PaginatedRequestParams::default().with_cursor(inner_cursor)),
                        context,
                    )
                    .await?;
                Ok(listing::Page {
                    items: result.tools,
                    next_cursor: result.next_cursor,
                })
            })
        })
        .await
    }

    /// Maps every composed provider's tool names to the provider that listed them, via
    /// the shared [`listing::index_by_provider`] skeleton.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-key error (kind `"tool"`) for the first tool name found
    /// listed by two different providers (by 0-indexed construction order).
    async fn index_tool_names_by_provider(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<HashMap<String, usize>, ErrorData> {
        let drained = self.drain_tools(context).await?;
        listing::index_by_provider(
            drained,
            "tool",
            |tool| tool.name.to_string(),
            |name| name.clone(),
        )
    }

    /// Looks up the composed provider that created `task_id`.
    ///
    /// # Errors
    ///
    /// Returns `Err(ErrorData::new(ErrorCode::INVALID_PARAMS, ...))` naming `task_id` when
    /// no [`MergedToolsProvider::call_tool`] call recorded it (never created, or created by
    /// a `MergedToolsProvider` this one is not).
    fn owning_provider_index(&self, task_id: &str) -> Result<usize, ErrorData> {
        self.provider_index_by_task_id
            .lock()
            .expect("provider_index_by_task_id mutex poisoned")
            .get(task_id)
            .copied()
            .ok_or_else(|| {
                ErrorData::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("no task with id {task_id:?} is known"),
                    None,
                )
            })
    }

    /// Removes `task_id` from [`MergedToolsProvider::provider_index_by_task_id`], once
    /// its owning provider answers a terminal status (`get_task`) or a successful
    /// cancellation (`cancel_task`).
    fn evict_task_id(&self, task_id: &str) {
        self.provider_index_by_task_id
            .lock()
            .expect("provider_index_by_task_id mutex poisoned")
            .remove(task_id);
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
        let merge_cursor = request.as_ref().and_then(|params| params.cursor.as_deref());

        // Every call re-validates the whole composition: a duplicate introduced by a
        // provider whose own tool list changed between calls must surface here too.
        // Skipped for an empty composition: there is nothing to list or to duplicate.
        if !self.providers.is_empty() {
            self.index_tool_names_by_provider(&context).await?;
        }

        let (tools, next_cursor) = listing::list_one_page(
            merge_cursor,
            self.providers.len(),
            |provider_index, inner_cursor| {
                let provider = &self.providers[provider_index];
                let context = context.clone();
                Box::pin(async move {
                    let result = provider
                        .list_tools(
                            Some(PaginatedRequestParams::default().with_cursor(inner_cursor)),
                            context,
                        )
                        .await?;
                    Ok(listing::Page {
                        items: result.tools,
                        next_cursor: result.next_cursor,
                    })
                })
            },
        )
        .await?;

        let mut result = ListToolsResult::with_all_items(tools);
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

        let response = provider.call_tool(request, context).await;

        if let Ok(CallToolResponse::Task(ref created)) = response {
            let task_id = created.task.task_id.clone();
            let mut provider_index_by_task_id = self
                .provider_index_by_task_id
                .lock()
                .expect("provider_index_by_task_id mutex poisoned");
            match provider_index_by_task_id.get(&task_id) {
                Some(&existing_provider_index) if existing_provider_index != provider_index => {
                    return Err(task_id_collision_error(
                        &task_id,
                        existing_provider_index,
                        provider_index,
                    ));
                }
                _ => {
                    provider_index_by_task_id.insert(task_id, provider_index);
                }
            }
        }

        response
    }

    /// Evicts `request.task_id` from the task-id-to-provider map once the owning
    /// provider answers a terminal [`TaskStatus`](rmcp::model::TaskStatus) (completed,
    /// failed, or cancelled): a terminal task never resumes, so its record would
    /// otherwise outlive any future `get_task`, `update_task`, or `cancel_task` call
    /// that could use it.
    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        let provider_index = self.owning_provider_index(&request.task_id)?;
        let provider = self
            .providers
            .get(provider_index)
            .expect("owning_provider_index only returns in-range provider indices");

        let task_id = request.task_id.clone();
        let result = provider.get_task(request, context).await?;
        if result.task.status().is_terminal() {
            self.evict_task_id(&task_id);
        }
        Ok(result)
    }

    // `update_task` does not evict `task_id` on a terminal status: the trait's
    // `update_task` answers `Result<(), ErrorData>` (SEP-2663's `tasks/update`
    // acknowledgement), with no task status to inspect. `get_task` and `cancel_task`
    // are the eviction paths; see their doc comments.
    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let provider_index = self.owning_provider_index(&request.task_id)?;
        let provider = self
            .providers
            .get(provider_index)
            .expect("owning_provider_index only returns in-range provider indices");

        provider.update_task(request, context).await
    }

    /// Evicts `request.task_id` from the task-id-to-provider map once the owning
    /// provider's `cancel_task` succeeds: cancellation is itself a terminal status.
    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let provider_index = self.owning_provider_index(&request.task_id)?;
        let provider = self
            .providers
            .get(provider_index)
            .expect("owning_provider_index only returns in-range provider indices");

        let task_id = request.task_id.clone();
        provider.cancel_task(request, context).await?;
        self.evict_task_id(&task_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rmcp::model::{
        CallToolResult, ContentBlock, CreateTaskResult, RequestId, Task, TaskStatus, Tool,
    };
    use rmcp::service::Peer;

    use super::super::cursor;
    use super::*;

    /// A fixed task id every [`TaskStub`] creates, so a test can route by it without a
    /// handshake with the real id generator.
    const FIXED_TASK_ID: &str = "fixed-id";

    /// How often each [`TaskStub`] method was reached, shared with the test that moves the
    /// stub into a [`MergedToolsProvider`] composition.
    struct TaskStubCalls {
        get_task: Mutex<usize>,
        update_task: Mutex<usize>,
        cancel_task: Mutex<usize>,
        /// The [`TaskStatus`] the owning [`TaskStub`]'s `get_task` answers; a test sets
        /// this to a terminal status to exercise [`MergedToolsProvider`]'s task-id
        /// eviction.
        get_task_status: Mutex<TaskStatus>,
    }

    impl Default for TaskStubCalls {
        fn default() -> Self {
            Self {
                get_task: Mutex::new(0),
                update_task: Mutex::new(0),
                cancel_task: Mutex::new(0),
                get_task_status: Mutex::new(TaskStatus::Working),
            }
        }
    }

    /// A [`ToolsProvider`] whose `call_tool` always materializes a task with
    /// [`FIXED_TASK_ID`], and whose `get_task`/`update_task`/`cancel_task` each count how
    /// often they are reached, so a test can assert that `MergedToolsProvider` routed a
    /// `tasks/*` call here rather than to another composed provider.
    struct TaskStub {
        tool_name: &'static str,
        calls: std::sync::Arc<TaskStubCalls>,
    }

    impl TaskStub {
        /// Builds a `TaskStub` listing tool `"make-task"`, and a shared handle to its
        /// call counters.
        fn new() -> (Self, std::sync::Arc<TaskStubCalls>) {
            Self::with_tool_name("make-task")
        }

        /// Builds a `TaskStub` listing `tool_name`, and a shared handle to its call
        /// counters. Every `TaskStub`, regardless of its tool name, materializes a task
        /// with the same [`FIXED_TASK_ID`], and answers `get_task` with
        /// [`TaskStatus::Working`] until the shared `calls.get_task_status` changes it.
        fn with_tool_name(tool_name: &'static str) -> (Self, std::sync::Arc<TaskStubCalls>) {
            let calls = std::sync::Arc::new(TaskStubCalls::default());
            (
                Self {
                    tool_name,
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    impl ToolsProvider for TaskStub {
        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult::with_all_items(vec![tool(self.tool_name)]))
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            let task = Task::new(
                FIXED_TASK_ID,
                TaskStatus::Working,
                "2026-10-08T00:00:00Z",
                "2026-10-08T00:00:00Z",
            );
            Ok(CreateTaskResult::new(task).into())
        }

        async fn get_task(
            &self,
            request: GetTaskParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<GetTaskResult, ErrorData> {
            *self.calls.get_task.lock().expect("lock") += 1;
            let status = *self.calls.get_task_status.lock().expect("lock");
            let task = Task::new(
                request.task_id,
                status,
                "2026-10-08T00:00:00Z",
                "2026-10-08T00:00:00Z",
            );
            let payload = match status {
                TaskStatus::Completed => rmcp::model::TaskPayload::Completed {
                    result: rmcp::model::JsonObject::new(),
                },
                TaskStatus::Failed => rmcp::model::TaskPayload::Failed {
                    error: rmcp::model::JsonObject::new(),
                },
                TaskStatus::Cancelled => rmcp::model::TaskPayload::Cancelled,
                _ => rmcp::model::TaskPayload::Working,
            };
            Ok(GetTaskResult::new(rmcp::model::DetailedTask::new(
                task, payload,
            )))
        }

        async fn update_task(
            &self,
            _request: UpdateTaskParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            *self.calls.update_task.lock().expect("lock") += 1;
            Ok(())
        }

        async fn cancel_task(
            &self,
            _request: CancelTaskParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            *self.calls.cancel_task.lock().expect("lock") += 1;
            Ok(())
        }
    }

    /// An in-memory [`ToolsProvider`] stub serving fixed pages, by cursor, in order.
    ///
    /// `pages` is consumed front-to-back: the first page answers a `None` cursor, and
    /// each page's own `Option<String>` cursor value is the one the *next* `list_tools`
    /// call must present for `Stub` to serve the following page. `call_tool` answers its
    /// own `label` tagged together with the requested name, so a test can assert which
    /// stub a routed call reached, not merely that some stub answered.
    struct Stub {
        label: &'static str,
        pages: Mutex<Vec<(Vec<Tool>, Option<String>)>>,
    }

    impl Stub {
        fn new(label: &'static str, pages: Vec<(Vec<Tool>, Option<String>)>) -> Self {
            Self {
                label,
                pages: Mutex::new(pages),
            }
        }

        /// A stub with every tool on a single page (`next_cursor: None`).
        fn single_page(label: &'static str, tools: Vec<Tool>) -> Self {
            Self::new(label, vec![(tools, None)])
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
                "provider {label} ran {name}",
                label = self.label,
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
            .with_provider(Stub::single_page("A", vec![tool("a1"), tool("a2")]))
            .with_provider(Stub::single_page("B", vec![tool("b1"), tool("b2")]));

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
            .with_provider(Stub::new(
                "A",
                vec![
                    (vec![tool("a1")], Some("a-page-2".to_string())),
                    (vec![tool("a2")], None),
                ],
            ))
            .with_provider(Stub::new(
                "B",
                vec![
                    (vec![tool("b1")], Some("b-page-2".to_string())),
                    (vec![tool("b2")], None),
                ],
            ));

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
            .with_provider(Stub::single_page("A", vec![tool("shared")]))
            .with_provider(Stub::single_page("B", vec![tool("shared")]));

        let error = ToolsProvider::list_tools(&merged, None, test_context().await)
            .await
            .expect_err("list_tools reports the duplicate");

        assert!(error.message.contains("shared"), "{error:?}");
    }

    #[tokio::test]
    async fn routes_call_tool_to_the_provider_that_lists_the_name() {
        let merged = MergedToolsProvider::new()
            .with_provider(Stub::single_page("A", vec![tool("a1")]))
            .with_provider(Stub::single_page("B", vec![tool("b1")]));

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
        assert_eq!(text.text, "provider B ran b1");

        let response = ToolsProvider::call_tool(
            &merged,
            CallToolRequestParams::new("a1"),
            test_context().await,
        )
        .await
        .expect("call_tool succeeds");

        let CallToolResponse::Complete(result) = response else {
            panic!("expected a complete result");
        };
        let text = result.content[0].as_text().expect("text content");
        assert_eq!(text.text, "provider A ran a1");
    }

    #[tokio::test]
    async fn rejects_a_call_to_an_unknown_tool_name() {
        let merged =
            MergedToolsProvider::new().with_provider(Stub::single_page("A", vec![tool("a1")]));

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
    async fn routes_tasks_to_the_provider_that_created_them() {
        let (task_stub, task_stub_calls) = TaskStub::new();
        let merged = MergedToolsProvider::new()
            .with_provider(Stub::single_page("A", vec![tool("a1")]))
            .with_provider(task_stub);

        ToolsProvider::call_tool(
            &merged,
            CallToolRequestParams::new("make-task"),
            test_context().await,
        )
        .await
        .expect("call_tool succeeds");

        ToolsProvider::get_task(
            &merged,
            GetTaskParams::new(FIXED_TASK_ID),
            test_context().await,
        )
        .await
        .expect("get_task reaches the owning provider");
        ToolsProvider::update_task(
            &merged,
            UpdateTaskParams::new(FIXED_TASK_ID, rmcp::model::InputResponses::default()),
            test_context().await,
        )
        .await
        .expect("update_task reaches the owning provider");
        ToolsProvider::cancel_task(
            &merged,
            CancelTaskParams::new(FIXED_TASK_ID),
            test_context().await,
        )
        .await
        .expect("cancel_task reaches the owning provider");

        assert_eq!(*task_stub_calls.get_task.lock().expect("lock"), 1);
        assert_eq!(*task_stub_calls.update_task.lock().expect("lock"), 1);
        assert_eq!(*task_stub_calls.cancel_task.lock().expect("lock"), 1);
    }

    #[tokio::test]
    async fn a_terminal_get_task_evicts_the_task_id_record() {
        let (task_stub, task_stub_calls) = TaskStub::new();
        let merged = MergedToolsProvider::new().with_provider(task_stub);

        ToolsProvider::call_tool(
            &merged,
            CallToolRequestParams::new("make-task"),
            test_context().await,
        )
        .await
        .expect("call_tool succeeds");

        *task_stub_calls.get_task_status.lock().expect("lock") = TaskStatus::Completed;
        ToolsProvider::get_task(
            &merged,
            GetTaskParams::new(FIXED_TASK_ID),
            test_context().await,
        )
        .await
        .expect("get_task succeeds and observes the terminal status");

        let error = ToolsProvider::get_task(
            &merged,
            GetTaskParams::new(FIXED_TASK_ID),
            test_context().await,
        )
        .await
        .expect_err("the task-id record was evicted after the terminal get_task");
        assert!(error.message.contains(FIXED_TASK_ID), "{error:?}");
    }

    #[tokio::test]
    async fn a_successful_cancel_task_evicts_the_task_id_record() {
        let (task_stub, _task_stub_calls) = TaskStub::new();
        let merged = MergedToolsProvider::new().with_provider(task_stub);

        ToolsProvider::call_tool(
            &merged,
            CallToolRequestParams::new("make-task"),
            test_context().await,
        )
        .await
        .expect("call_tool succeeds");

        ToolsProvider::cancel_task(
            &merged,
            CancelTaskParams::new(FIXED_TASK_ID),
            test_context().await,
        )
        .await
        .expect("cancel_task succeeds");

        let error = ToolsProvider::get_task(
            &merged,
            GetTaskParams::new(FIXED_TASK_ID),
            test_context().await,
        )
        .await
        .expect_err("the task-id record was evicted after the successful cancel_task");
        assert!(error.message.contains(FIXED_TASK_ID), "{error:?}");
    }

    #[tokio::test]
    async fn a_task_id_collision_from_a_different_provider_is_refused() {
        let (task_stub_a, task_stub_a_calls) = TaskStub::with_tool_name("make-task-a");
        let (task_stub_b, _task_stub_b_calls) = TaskStub::with_tool_name("make-task-b");
        let merged = MergedToolsProvider::new()
            .with_provider(task_stub_a)
            .with_provider(task_stub_b);

        ToolsProvider::call_tool(
            &merged,
            CallToolRequestParams::new("make-task-a"),
            test_context().await,
        )
        .await
        .expect("the first call_tool succeeds and records provider 0 as the owner");

        let error = ToolsProvider::call_tool(
            &merged,
            CallToolRequestParams::new("make-task-b"),
            test_context().await,
        )
        .await
        .expect_err("a task id already owned by a different provider is refused");
        assert!(error.message.contains(FIXED_TASK_ID), "{error:?}");
        assert!(error.message.contains("provider 0"), "{error:?}");
        assert!(error.message.contains("provider 1"), "{error:?}");

        ToolsProvider::get_task(
            &merged,
            GetTaskParams::new(FIXED_TASK_ID),
            test_context().await,
        )
        .await
        .expect("tasks/get still reaches the first, unchanged owner");
        assert_eq!(*task_stub_a_calls.get_task.lock().expect("lock"), 1);
    }

    #[tokio::test]
    async fn rejects_tasks_calls_for_an_unknown_task_id() {
        let (task_stub, _task_stub_calls) = TaskStub::new();
        let merged = MergedToolsProvider::new()
            .with_provider(Stub::single_page("A", vec![tool("a1")]))
            .with_provider(task_stub);

        const UNKNOWN_TASK_ID: &str = "unknown-id";

        let get_task_error = ToolsProvider::get_task(
            &merged,
            GetTaskParams::new(UNKNOWN_TASK_ID),
            test_context().await,
        )
        .await
        .expect_err("get_task reports the unknown id");
        let update_task_error = ToolsProvider::update_task(
            &merged,
            UpdateTaskParams::new(UNKNOWN_TASK_ID, rmcp::model::InputResponses::default()),
            test_context().await,
        )
        .await
        .expect_err("update_task reports the unknown id");
        let cancel_task_error = ToolsProvider::cancel_task(
            &merged,
            CancelTaskParams::new(UNKNOWN_TASK_ID),
            test_context().await,
        )
        .await
        .expect_err("cancel_task reports the unknown id");

        for error in [get_task_error, update_task_error, cancel_task_error] {
            assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
            assert!(error.message.contains(UNKNOWN_TASK_ID), "{error:?}");
        }
    }

    #[tokio::test]
    async fn rejects_a_call_to_a_name_shared_by_two_providers() {
        let merged = MergedToolsProvider::new()
            .with_provider(Stub::single_page("A", vec![tool("shared")]))
            .with_provider(Stub::single_page("B", vec![tool("shared")]));

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
