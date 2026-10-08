//! End-to-end tests: a real `rmcp` client drives a `MergedPromptsProvider` composition,
//! over a duplex transport, the same way `tests/composed_server.rs` drives a single
//! provider. These tests assert on the client-visible response shape only.

use std::sync::Mutex;

use rmcp::{
    ServiceExt,
    model::{
        ClientCapabilities, ClientConfig, ErrorCode, GetPromptRequestParams, GetPromptResponse,
        GetPromptResult, Implementation, PaginatedRequestParams, Prompt, PromptMessage,
        ProtocolVersion, Role,
    },
    service::{RequestContext, RoleServer, ServiceError},
};
use rmcp_server_builder::{MergedPromptsProvider, PromptsProvider, ServerBuilder};

/// An in-memory [`PromptsProvider`] stub serving fixed prompt pages, by cursor, in
/// order. `get_prompt` answers its own `label` tagged together with the requested
/// name, so a test can assert which stub a routed call reached, not merely that some
/// stub answered.
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
    ) -> Result<rmcp::model::ListPromptsResult, rmcp::model::ErrorData> {
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
        let mut result = rmcp::model::ListPromptsResult::with_all_items(prompts);
        result.next_cursor = next_cursor;
        Ok(result)
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, rmcp::model::ErrorData> {
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
async fn client_lists_both_providers_prompt_sets_concatenated() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .prompts(
            MergedPromptsProvider::new()
                .with_provider(Stub::single_page("A", vec![prompt("a1"), prompt("a2")]))
                .with_provider(Stub::single_page("B", vec![prompt("b1"), prompt("b2")])),
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

    let mut all_prompt_names = Vec::new();
    let mut cursor = None;
    loop {
        let request = cursor
            .take()
            .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
        let result = client.list_prompts(request).await.expect("list_prompts");
        all_prompt_names.extend(result.prompts.iter().map(|prompt| prompt.name.clone()));
        match result.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    assert_eq!(all_prompt_names, vec!["a1", "a2", "b1", "b2"]);

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn get_prompt_reaches_the_provider_that_lists_the_name() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .prompts(
            MergedPromptsProvider::new()
                .with_provider(Stub::single_page("A", vec![prompt("a1")]))
                .with_provider(Stub::single_page("B", vec![prompt("b1")])),
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
        .get_prompt(GetPromptRequestParams::new("b1"))
        .await
        .expect("get_prompt");
    let text = result.messages[0].content.as_text().expect("text content");
    assert_eq!(text.text, "provider B ran b1");

    let result = client
        .get_prompt(GetPromptRequestParams::new("a1"))
        .await
        .expect("get_prompt");
    let text = result.messages[0].content.as_text().expect("text content");
    assert_eq!(text.text, "provider A ran a1");

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn a_shared_prompt_name_surfaces_as_a_client_visible_error() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .prompts(
            MergedPromptsProvider::new()
                .with_provider(Stub::single_page("A", vec![prompt("shared")]))
                .with_provider(Stub::single_page("B", vec![prompt("shared")])),
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

    let response = client.list_prompts(None).await;

    let Err(ServiceError::McpError(error)) = response else {
        panic!("expected an MCP error, got {response:?}");
    };
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert!(error.message.contains("shared"), "{error:?}");

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}
