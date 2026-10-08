//! End-to-end test: a real `rmcp` client's `tools/call` for a task-creating tool,
//! composed inside a `MergedToolsProvider` alongside a plain, non-task-creating stub,
//! surfaces the same `tasks/*`-naming internal error that
//! `tests/composed_server.rs::a_task_from_a_tools_provider_is_answered_with_an_internal_error`
//! already asserts for a bare (non-merged) provider.
//!
//! # Why this file stops short of a real `tasks/get` round trip
//!
//! `Server::call_tool` (`src/server.rs`) unconditionally converts any
//! `CallToolResponse::Task` the composed tools provider answers into
//! `ErrorData::internal_error(TASK_NOT_SERVED_MESSAGE, ...)`, *before* a real client
//! ever receives a task id:
//!
//! ```ignore
//! Ok(CallToolResponse::Task(_)) => Err(ErrorData::internal_error(TASK_NOT_SERVED_MESSAGE, None)),
//! ```
//!
//! This holds for every tools provider the composed `Server` wraps, merged or not, and
//! is unconditional on the client's declared capabilities (rmcp-server-builder#5, a
//! pre-existing, documented limitation predating this branch's merge-providers work).
//! `MergedToolsProvider::get_task`/`update_task`/`cancel_task` do route a `tasks/*` call
//! to the composed provider that created the task (see
//! `src/merge/tools.rs::tests::routes_tasks_to_the_provider_that_created_them`, which
//! exercises that routing directly against the trait), but no real client can ever
//! reach that routing through a `ServerBuilder`-built `Server`: no task is ever
//! created over the wire to poll with `tasks/get`. This test instead asserts the one
//! thing that IS observable end-to-end: that `MergedToolsProvider` composition does
//! not change this pre-existing guard's behavior for a task-creating tool, and that
//! the plain tool composed alongside it is unaffected.

use rmcp::{
    ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig,
        ContentBlock, CreateTaskResult, ErrorCode, ErrorData, GetTaskParams, GetTaskResult,
        Implementation, JsonObject, ListToolsResult, PaginatedRequestParams, RequestId, Task,
        TaskPayload, TaskStatus, Tool,
    },
    service::{Peer, RequestContext, RoleServer, ServiceError},
};
use rmcp_server_builder::{MergedToolsProvider, ServerBuilder, ToolsProvider};

/// A distinguishable marker [`TaskTools::get_task`] stamps onto the
/// [`GetTaskResult`]'s task status message, so a test can assert that the real
/// client's `tasks/get` call reached this stub's own state, not the plain stub's.
const STATUS_MESSAGE: &str = "created-by-task-tools";

/// A fixed task id [`TaskTools::call_tool`] always creates.
const FIXED_TASK_ID: &str = "task-1";

/// A [`ToolsProvider`] whose `call_tool` materializes a task (SEP-2663), and whose
/// `get_task` answers a [`GetTaskResult`] carrying [`STATUS_MESSAGE`], so a test can
/// tell that `MergedToolsProvider` routed `tasks/get` to this provider rather than to
/// the plain stub composed alongside it.
struct TaskTools;

impl ToolsProvider for TaskTools {
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "make-task",
            "creates a task",
            JsonObject::new(),
        )]))
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
        let task = Task::new(
            request.task_id,
            TaskStatus::Working,
            "2026-10-08T00:00:00Z",
            "2026-10-08T00:00:00Z",
        )
        .with_status_message(STATUS_MESSAGE);
        Ok(GetTaskResult::new(rmcp::model::DetailedTask::new(
            task,
            TaskPayload::Working,
        )))
    }
}

/// A plain [`ToolsProvider`] whose `call_tool` never creates a task, composed
/// alongside [`TaskTools`] to prove `tasks/get` routes to the owning provider and not
/// to this one.
struct PlainTool;

impl ToolsProvider for PlainTool {
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "plain",
            "a plain tool",
            JsonObject::new(),
        )]))
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        Ok(CallToolResult::success(vec![ContentBlock::text("ran plain")]).into())
    }
}

#[tokio::test]
async fn a_task_creating_tool_composed_with_a_plain_tool_still_answers_an_internal_error() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .tools(
            MergedToolsProvider::new()
                .with_provider(PlainTool)
                .with_provider(TaskTools),
        )
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let mut configuration = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("test-client", "1.0.0"),
    );
    configuration.capabilities = ClientCapabilities::builder().enable_tasks().build();
    let client = configuration
        .serve(client_transport)
        .await
        .expect("initialize client");

    // The plain tool, composed alongside the task-creating one, is unaffected.
    let plain_result = client
        .call_tool(CallToolRequestParams::new("plain"))
        .await
        .expect("call_tool");
    let text = plain_result.content[0].as_text().expect("text content");
    assert_eq!(text.text, "ran plain");

    // `Server::call_tool` (src/server.rs) unconditionally converts a
    // `CallToolResponse::Task` to an internal error naming `tasks/*`, for any composed
    // tools provider (see this file's module doc comment): composing the task-creating
    // tool inside a `MergedToolsProvider` does not change that.
    let response = client
        .call_tool_once(CallToolRequestParams::new("make-task"))
        .await;
    let Err(ServiceError::McpError(error)) = response else {
        panic!("expected an MCP error, got {response:?}");
    };
    assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
    assert!(
        error.message.contains("tasks/"),
        "message names tasks/*: {}",
        error.message
    );

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

/// A [`RequestContext<RoleServer>`] usable in a pure unit test, mirroring the
/// construction each merge module's own unit tests use (see e.g.
/// `src/merge/tools.rs::tests::test_context`): `rmcp`'s `Peer` has no public
/// constructor outside a live client/server handshake, so this mints one over an
/// in-memory duplex transport and then discards the connection.
async fn test_context() -> RequestContext<RoleServer> {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("tasks-routing-test", "0.0.0"))
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        let peer: Peer<RoleServer> = running.peer().clone();
        running.waiting().await.expect("server stops");
        peer
    });

    let client_configuration = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("tasks-routing-test-client", "0.0.0"),
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
async fn get_task_routes_through_the_public_tools_provider_trait_to_the_creating_provider() {
    // `Server::call_tool` blocks a real client from ever reaching this routing over the
    // wire (see this file's module doc comment), but `MergedToolsProvider` itself,
    // used only through its public `ToolsProvider` API, does route `tasks/get` to the
    // composed provider that created the task, not to the plain one composed
    // alongside it. This exercises that public-API contract directly.
    let merged = MergedToolsProvider::new()
        .with_provider(PlainTool)
        .with_provider(TaskTools);

    ToolsProvider::call_tool(
        &merged,
        CallToolRequestParams::new("make-task"),
        test_context().await,
    )
    .await
    .expect("call_tool succeeds");

    let info = ToolsProvider::get_task(
        &merged,
        GetTaskParams::new(FIXED_TASK_ID),
        test_context().await,
    )
    .await
    .expect("get_task reaches the owning provider");

    assert_eq!(
        info.task.task.status_message.as_deref(),
        Some(STATUS_MESSAGE),
        "get_task must reach TaskTools's own state, not PlainTool's"
    );
}
