//! End-to-end tests: a real `rmcp` client drives a `MergedCompletionProvider` and a
//! `MergedLoggingProvider` composition, over a duplex transport, the same way
//! `tests/composed_server.rs` drives a single provider. These tests assert on the
//! client-visible response shape only.

use std::sync::Mutex;

use rmcp::{
    ServiceExt,
    model::{
        ArgumentInfo, ClientCapabilities, ClientConfig, CompleteRequestParams, CompleteResult,
        CompletionInfo, ErrorData, GetPromptRequestParams, GetPromptResponse, GetPromptResult,
        Implementation, ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult,
        PaginatedRequestParams, Prompt, PromptMessage, ReadResourceRequestParams,
        ReadResourceResponse, Reference, ResourceTemplate, Role, SubscribeRequestParams,
        UnsubscribeRequestParams,
    },
    service::{RequestContext, RoleServer},
};
use rmcp_server_builder::{
    CompletionProvider, LoggingProvider, MergedCompletionProvider, MergedLoggingProvider,
    PromptsProvider, ResourcesProvider, ServerBuilder,
};

#[expect(
    deprecated,
    reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
)]
use rmcp::model::{LoggingLevel, SetLevelRequestParams};

/// A completion source that is both a [`PromptsProvider`] and a [`ResourcesProvider`]
/// (only one listing matters per test) and a [`CompletionProvider`], answering a fixed,
/// distinguishable [`CompleteResult`] marked with [`CompletionSource::marker`].
struct CompletionSource {
    prompt_names: Vec<&'static str>,
    resource_templates: Vec<&'static str>,
    marker: &'static str,
}

impl CompletionSource {
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

    fn marked_result(&self) -> CompleteResult {
        CompleteResult::new(
            CompletionInfo::new(vec![self.marker.to_string()])
                .expect("one completion value is under CompletionInfo::MAX_VALUES"),
        )
    }
}

impl PromptsProvider for CompletionSource {
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
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            Role::Assistant,
            format!("ran {name}", name = request.name),
        )])
        .into())
    }
}

impl ResourcesProvider for CompletionSource {
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(Vec::new()))
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
        unimplemented!("not exercised by this file's tests")
    }

    async fn subscribe(
        &self,
        _request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        unimplemented!("not exercised by this file's tests")
    }

    async fn unsubscribe(
        &self,
        _request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        unimplemented!("not exercised by this file's tests")
    }
}

impl CompletionProvider for CompletionSource {
    async fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        Ok(self.marked_result())
    }
}

/// A [`LoggingProvider`] stub recording the last [`SetLevelRequestParams`] it saw,
/// shared with the test that moves the stub into a [`MergedLoggingProvider`]
/// composition.
struct LoggingStub {
    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    last_level: std::sync::Arc<Mutex<Option<LoggingLevel>>>,
}

impl LoggingStub {
    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    fn new() -> (Self, std::sync::Arc<Mutex<Option<LoggingLevel>>>) {
        let last_level = std::sync::Arc::new(Mutex::new(None));
        (
            Self {
                last_level: last_level.clone(),
            },
            last_level,
        )
    }
}

impl LoggingProvider for LoggingStub {
    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    async fn set_level(
        &self,
        request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        *self.last_level.lock().expect("last_level lock") = Some(request.level);
        Ok(())
    }
}

fn client_configuration() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("test-client", "1.0.0"),
    )
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

#[tokio::test]
async fn a_prompt_name_reference_reaches_the_source_that_lists_it_via_real_client() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .completion(
            MergedCompletionProvider::new()
                .with_prompt_source(CompletionSource::with_prompt_names("a", vec!["a1"]))
                .with_prompt_source(CompletionSource::with_prompt_names("b", vec!["b1"])),
        )
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let client = client_configuration()
        .serve(client_transport)
        .await
        .expect("initialize client");

    let result = client
        .complete(prompt_request("b1"))
        .await
        .expect("complete");
    assert_eq!(result.completion.values, vec!["b".to_string()]);

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn a_resource_template_uri_reference_reaches_the_source_that_lists_it_via_real_client() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .completion(
            MergedCompletionProvider::new()
                .with_resource_source(CompletionSource::with_resource_templates(
                    "a",
                    vec!["a://{id}"],
                ))
                .with_resource_source(CompletionSource::with_resource_templates(
                    "b",
                    vec!["b://{id}"],
                )),
        )
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let client = client_configuration()
        .serve(client_transport)
        .await
        .expect("initialize client");

    let result = client
        .complete(resource_request("b://{id}"))
        .await
        .expect("complete");
    assert_eq!(result.completion.values, vec!["b".to_string()]);

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[expect(
    deprecated,
    reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
)]
#[tokio::test]
async fn set_level_reaches_every_composed_provider_via_real_client() {
    let (first, first_state) = LoggingStub::new();
    let (second, second_state) = LoggingStub::new();
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .logging(
            MergedLoggingProvider::new()
                .with_provider(first)
                .with_provider(second),
        )
        .build();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });

    let client = client_configuration()
        .serve(client_transport)
        .await
        .expect("initialize client");

    client
        .set_level(SetLevelRequestParams::new(LoggingLevel::Warning))
        .await
        .expect("set_level");

    assert_eq!(
        *first_state.lock().expect("lock"),
        Some(LoggingLevel::Warning)
    );
    assert_eq!(
        *second_state.lock().expect("lock"),
        Some(LoggingLevel::Warning)
    );

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}
