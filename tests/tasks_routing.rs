//! End-to-end test: a real `rmcp` client's `tools/call` for a task-creating tool,
//! composed inside a `MergedToolsProvider` alongside a plain, non-task-creating stub,
//! round-trips through a real `tasks/get` call that reaches the creating provider.
//!
//! `Server::call_tool` (`src/server.rs`) passes a `CallToolResponse::Task` through
//! unchanged, so a real client receives the task id and can poll it with `tasks/get`.
//! `Server::get_task` delegates to the composed tools provider, and
//! `MergedToolsProvider::get_task` routes to the specific provider that created the
//! task (see `src/merge/tools.rs::tests::routes_tasks_to_the_provider_that_created_them`
//! for the same routing exercised directly against the trait). This test proves that
//! routing end-to-end, through the wire, with a plain tool composed alongside the
//! task-creating one to show the response reaches the owning provider and not it.

use rmcp::{
    ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig,
        ContentBlock, CreateTaskResult, ErrorData, GetTaskParams, GetTaskResult, Implementation,
        JsonObject, ListToolsResult, PaginatedRequestParams, RequestId, ServerCapabilities, Task,
        TaskPayload, TaskStatus, Tool,
    },
    service::{Peer, RequestContext, RoleServer},
};
use rmcp_server_builder::{MergedToolsProvider, ServerBuilder, SimpleInfo, ToolsProvider};

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
async fn a_task_creating_tool_composed_with_a_plain_tool_routes_tasks_get_to_the_creating_provider()
{
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    // `tasks/get` is gated on the server advertising the tasks extension
    // (SEP-2663, `rmcp`'s `validate_tasks_capability`): without it, `rmcp` answers
    // `METHOD_NOT_FOUND` before ever reaching `Server::get_task`.
    let info = SimpleInfo::new(Implementation::new("test-server", "1.0.0"))
        .with_capabilities(ServerCapabilities::builder().enable_tasks().build());
    let server = ServerBuilder::new()
        .info(info)
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

    // `Server::call_tool` passes a `CallToolResponse::Task` through unchanged: the real
    // client receives the task id `TaskTools::call_tool` minted.
    let CallToolResponse::Task(create_task_result) = client
        .call_tool_once(CallToolRequestParams::new("make-task"))
        .await
        .expect("call_tool succeeds and returns a task")
    else {
        panic!("expected a CallToolResponse::Task");
    };

    // `Server::get_task` delegates to the composed `MergedToolsProvider`, which routes
    // `tasks/get` to `TaskTools`, the specific provider that created the task, not to
    // `PlainTool` composed alongside it. `STATUS_MESSAGE` proves which provider answered.
    let task_result = client
        .get_task(GetTaskParams::new(create_task_result.task.task_id))
        .await
        .expect("tasks/get reaches the owning provider");
    assert_eq!(
        task_result.task.task.status_message.as_deref(),
        Some(STATUS_MESSAGE),
        "tasks/get must reach TaskTools's own state, not PlainTool's"
    );

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn tools_call_answers_an_error_for_a_task_when_tasks_are_not_advertised() {
    // No `enable_tasks()` on either side: the composed server's combined capabilities
    // do not advertise SEP-2663, so `Server::call_tool` (`src/server.rs`) must refuse
    // `TaskTools::call_tool`'s `CallToolResponse::Task` instead of forwarding it; a
    // client with no `tasks/*` support could never fetch the result otherwise.
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .tools(MergedToolsProvider::new().with_provider(TaskTools))
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let configuration = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("test-client", "1.0.0"),
    );
    let client = configuration
        .serve(client_transport)
        .await
        .expect("initialize client");

    let error = client
        .call_tool_once(CallToolRequestParams::new("make-task"))
        .await
        .expect_err("a task is refused when the server does not advertise tasks");
    assert!(error.to_string().contains("tasks"), "{error}");

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
