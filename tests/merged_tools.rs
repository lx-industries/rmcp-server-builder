//! End-to-end tests: a real `rmcp` client drives a `MergedToolsProvider` composition,
//! over a duplex transport, the same way `tests/composed_server.rs` drives a single
//! provider. These tests assert on the client-visible response shape only.

use std::sync::Mutex;

use rmcp::{
    ServiceExt,
    model::{
        CallToolRequestParams, ClientCapabilities, ClientConfig, ContentBlock, ErrorCode,
        Implementation, JsonObject, PaginatedRequestParams, ProtocolVersion, Tool,
    },
    service::{RequestContext, RoleServer, ServiceError},
};
use rmcp_server_builder::{MergedToolsProvider, ServerBuilder, ToolsProvider};

/// An in-memory [`ToolsProvider`] stub serving fixed tool pages, by cursor, in order.
///
/// `pages` is consumed front-to-back: the first page answers a `None` cursor, and each
/// page's own `Option<String>` cursor value is the one the *next* `list_tools` call
/// must present for this stub to serve the following page. `call_tool` answers its own
/// `label` tagged together with the requested name, so a test can assert which stub a
/// routed call reached, not merely that some stub answered.
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
    ) -> Result<rmcp::model::ListToolsResult, rmcp::model::ErrorData> {
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
        let mut result = rmcp::model::ListToolsResult::with_all_items(tools);
        result.next_cursor = next_cursor;
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::model::ErrorData> {
        Ok(
            rmcp::model::CallToolResult::success(vec![ContentBlock::text(format!(
                "provider {label} ran {name}",
                label = self.label,
                name = request.name
            ))])
            .into(),
        )
    }
}

fn tool(name: &'static str) -> Tool {
    Tool::new(name, "a test tool", JsonObject::new())
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
async fn client_lists_both_providers_tool_sets_concatenated() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .tools(
            MergedToolsProvider::new()
                .with_provider(Stub::single_page("A", vec![tool("a1"), tool("a2")]))
                .with_provider(Stub::single_page("B", vec![tool("b1"), tool("b2")])),
        )
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let client = client_configuration(ProtocolVersion::LATEST_WITH_INITIALIZE)
        .serve(client_transport)
        .await
        .expect("initialize client");

    let mut all_tool_names = Vec::new();
    let mut cursor = None;
    loop {
        let request = cursor
            .take()
            .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
        let result = client.list_tools(request).await.expect("list_tools");
        all_tool_names.extend(result.tools.iter().map(|tool| tool.name.to_string()));
        match result.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    assert_eq!(all_tool_names, vec!["a1", "a2", "b1", "b2"]);

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn call_tool_reaches_the_provider_that_lists_the_name() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .tools(
            MergedToolsProvider::new()
                .with_provider(Stub::single_page("A", vec![tool("a1")]))
                .with_provider(Stub::single_page("B", vec![tool("b1")])),
        )
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let client = client_configuration(ProtocolVersion::LATEST_WITH_INITIALIZE)
        .serve(client_transport)
        .await
        .expect("initialize client");

    let result = client
        .call_tool(CallToolRequestParams::new("b1"))
        .await
        .expect("call_tool");
    let text = result.content[0].as_text().expect("text content");
    assert_eq!(text.text, "provider B ran b1");

    let result = client
        .call_tool(CallToolRequestParams::new("a1"))
        .await
        .expect("call_tool");
    let text = result.content[0].as_text().expect("text content");
    assert_eq!(text.text, "provider A ran a1");

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn a_shared_tool_name_surfaces_as_a_client_visible_error() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .tools(
            MergedToolsProvider::new()
                .with_provider(Stub::single_page("A", vec![tool("shared")]))
                .with_provider(Stub::single_page("B", vec![tool("shared")])),
        )
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let client = client_configuration(ProtocolVersion::LATEST_WITH_INITIALIZE)
        .serve(client_transport)
        .await
        .expect("initialize client");

    let response = client.list_tools(None).await;

    let Err(ServiceError::McpError(error)) = response else {
        panic!("expected an MCP error, got {response:?}");
    };
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert!(error.message.contains("shared"), "{error:?}");

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn real_client_pages_through_four_pages_across_two_providers_in_order() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .tools(
            MergedToolsProvider::new()
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
                )),
        )
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let client = client_configuration(ProtocolVersion::LATEST_WITH_INITIALIZE)
        .serve(client_transport)
        .await
        .expect("initialize client");

    let mut seen_tool_names = Vec::new();
    let mut cursor = None;
    for _ in 0..4 {
        let request = Some(PaginatedRequestParams::default().with_cursor(cursor.clone()));
        let result = client.list_tools(request).await.expect("list_tools");
        assert_eq!(result.tools.len(), 1, "one provider-page per merge page");
        seen_tool_names.push(result.tools[0].name.to_string());
        cursor = result.next_cursor;
    }

    assert_eq!(seen_tool_names, vec!["a1", "a2", "b1", "b2"]);
    assert_eq!(cursor, None, "the fourth call ends pagination");

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}
