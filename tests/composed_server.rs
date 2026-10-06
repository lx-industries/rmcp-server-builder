//! End-to-end tests: an rmcp client talks to a composed `Server` over a duplex transport.

use rmcp::{
    ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig,
        ContentBlock, CreateTaskResult, ErrorCode, ErrorData, GetPromptRequestParams,
        GetPromptResponse, GetPromptResult, Implementation, ListPromptsResult,
        ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
        ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
        ResourceContents, SubscribeRequestParams, Task, TaskStatus, UnsubscribeRequestParams,
    },
    service::{RequestContext, RoleServer, ServiceError},
};
use rmcp_server_builder::{PromptsProvider, ResourcesProvider, ServerBuilder, ToolsProvider};

const TOOL_TEXT: &str = "tool ran";
const PROMPT_DESCRIPTION: &str = "a prompt";
const RESOURCE_URI: &str = "test://resource";
const RESOURCE_TEXT: &str = "resource body";

struct Tools;

impl ToolsProvider for Tools {
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::default())
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        Ok(CallToolResult::success(vec![ContentBlock::text(TOOL_TEXT)]).into())
    }
}

/// A tools provider whose `call_tool` materializes a task (SEP-2663).
struct TaskTools;

impl ToolsProvider for TaskTools {
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::default())
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let task = Task::new(
            "task-1",
            TaskStatus::Working,
            "2026-10-06T00:00:00Z",
            "2026-10-06T00:00:00Z",
        );
        Ok(CreateTaskResult::new(task).into())
    }
}

struct Prompts;

impl PromptsProvider for Prompts {
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::default())
    }

    async fn get_prompt(
        &self,
        _request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        Ok(GetPromptResult::new(vec![])
            .with_description(PROMPT_DESCRIPTION)
            .into())
    }
}

struct Resources;

impl ResourcesProvider for Resources {
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::default())
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::default())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        Ok(
            ReadResourceResult::new(vec![ResourceContents::text(RESOURCE_TEXT, request.uri)])
                .into(),
        )
    }

    async fn subscribe(
        &self,
        _request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Ok(())
    }

    async fn unsubscribe(
        &self,
        _request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Ok(())
    }
}

/// Builds a client configuration that requests `protocol_version` in `initialize`.
fn client_configuration(protocol_version: ProtocolVersion) -> ClientConfig {
    let mut configuration = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("test-client", "1.0.0"),
    );
    configuration.protocol_version = protocol_version;
    configuration
}

#[tokio::test]
async fn client_calls_tool_gets_prompt_and_reads_resource() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .tools(Tools)
        .prompts(Prompts)
        .resources(Resources)
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let client = client_configuration(ProtocolVersion::LATEST_WITH_INITIALIZE)
        .serve(client_transport)
        .await
        .expect("initialize client");

    let tool = client
        .call_tool(CallToolRequestParams::new("any"))
        .await
        .expect("call tool");
    let tool_text = tool.content[0].as_text().expect("text content");
    assert_eq!(tool_text.text, TOOL_TEXT);

    let prompt = client
        .get_prompt(GetPromptRequestParams::new("any"))
        .await
        .expect("get prompt");
    assert_eq!(prompt.description.as_deref(), Some(PROMPT_DESCRIPTION));

    let resource = client
        .read_resource(ReadResourceRequestParams::new(RESOURCE_URI))
        .await
        .expect("read resource");
    let ResourceContents::TextResourceContents { uri, text, .. } = &resource.contents[0] else {
        panic!("expected text resource contents");
    };
    assert_eq!(uri, RESOURCE_URI);
    assert_eq!(text, RESOURCE_TEXT);

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn initialize_answers_with_the_requested_supported_version() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .instructions("test instructions")
        .tools(Tools)
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let client = client_configuration(ProtocolVersion::V_2025_06_18)
        .serve(client_transport)
        .await
        .expect("initialize client");

    let peer_info = client.peer_info().expect("server peer info");
    assert_eq!(peer_info.protocol_version, ProtocolVersion::V_2025_06_18);
    assert_eq!(
        peer_info
            .server_info
            .as_ref()
            .map(|info| info.name.as_str()),
        Some("test-server")
    );
    assert_eq!(peer_info.instructions.as_deref(), Some("test instructions"));
    assert!(peer_info.capabilities.tools.is_some());

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn a_task_from_a_tools_provider_is_answered_with_an_internal_error() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .tools(TaskTools)
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let mut configuration = client_configuration(ProtocolVersion::LATEST_WITH_INITIALIZE);
    configuration.capabilities = ClientCapabilities::builder().enable_tasks().build();
    let client = configuration
        .serve(client_transport)
        .await
        .expect("initialize client");

    let response = client
        .call_tool_once(CallToolRequestParams::new("any"))
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
