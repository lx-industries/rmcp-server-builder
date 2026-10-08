//! End-to-end tests: a real `rmcp` client drives a `MergedResourcesProvider`
//! composition, over a duplex transport, the same way `tests/composed_server.rs`
//! drives a single provider. These tests assert on the client-visible response shape
//! only, except for a shared call log each stub pushes onto, to tell which stub a
//! routed call reached.

use std::sync::{Arc, Mutex};

use rmcp::{
    ServiceExt,
    model::{
        ClientCapabilities, ClientConfig, ErrorCode, Implementation, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
        ResourceTemplate, SubscribeRequestParams, UnsubscribeRequestParams,
    },
    service::{RequestContext, RoleServer, ServiceError},
};
use rmcp_server_builder::{MergedResourcesProvider, ResourcesProvider, ServerBuilder};

/// An in-memory [`ResourcesProvider`] stub listing a fixed resource set and a fixed
/// resource template set, both on a single page. `read_resource` and `subscribe` push
/// [`Stub::name`] onto a shared call log, so a test can assert which stub a routed
/// call reached, without inspecting internal composition state.
struct Stub {
    name: &'static str,
    resources: Vec<Resource>,
    resource_templates: Vec<ResourceTemplate>,
    call_log: Arc<Mutex<Vec<&'static str>>>,
}

impl Stub {
    fn new(
        name: &'static str,
        resources: Vec<Resource>,
        resource_templates: Vec<ResourceTemplate>,
        call_log: Arc<Mutex<Vec<&'static str>>>,
    ) -> Self {
        Self {
            name,
            resources,
            resource_templates,
            call_log,
        }
    }
}

impl ResourcesProvider for Stub {
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::model::ErrorData> {
        Ok(rmcp::model::ListResourcesResult::with_all_items(
            self.resources.clone(),
        ))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListResourceTemplatesResult, rmcp::model::ErrorData> {
        Ok(rmcp::model::ListResourceTemplatesResult::with_all_items(
            self.resource_templates.clone(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, rmcp::model::ErrorData> {
        self.call_log.lock().expect("call log lock").push(self.name);
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![rmcp::model::ResourceContents::text(
                format!("read by {name}", name = self.name),
                request.uri,
            )],
        )))
    }

    async fn subscribe(
        &self,
        _request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), rmcp::model::ErrorData> {
        self.call_log.lock().expect("call log lock").push(self.name);
        Ok(())
    }

    async fn unsubscribe(
        &self,
        _request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), rmcp::model::ErrorData> {
        unimplemented!("not exercised by this file's tests")
    }
}

fn resource(uri: &'static str) -> Resource {
    Resource::new(uri, uri)
}

fn resource_template(uri_template: &'static str) -> ResourceTemplate {
    ResourceTemplate::new(uri_template, uri_template)
}

fn client_configuration() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("test-client", "1.0.0"),
    )
}

#[tokio::test]
async fn uri_matching_only_one_providers_template_reaches_it_via_real_read_resource() {
    let call_log = Arc::new(Mutex::new(Vec::new()));
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .resources(
            MergedResourcesProvider::new()
                .with_provider(Stub::new(
                    "a",
                    Vec::new(),
                    vec![resource_template("a://{id}")],
                    call_log.clone(),
                ))
                .with_provider(Stub::new(
                    "b",
                    Vec::new(),
                    vec![resource_template("b://{id}")],
                    call_log.clone(),
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
        .read_resource(ReadResourceRequestParams::new("a://42"))
        .await
        .expect("read_resource");
    let rmcp::model::ResourceContents::TextResourceContents { text, .. } = &result.contents[0]
    else {
        panic!("expected text resource contents");
    };
    assert_eq!(text, "read by a");
    assert_eq!(*call_log.lock().expect("call log lock"), vec!["a"]);

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn client_lists_both_providers_resources_via_real_client() {
    let call_log = Arc::new(Mutex::new(Vec::new()));
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .resources(
            MergedResourcesProvider::new()
                .with_provider(Stub::new(
                    "a",
                    vec![resource("a://1"), resource("a://2")],
                    Vec::new(),
                    call_log.clone(),
                ))
                .with_provider(Stub::new(
                    "b",
                    vec![resource("b://1"), resource("b://2")],
                    Vec::new(),
                    call_log.clone(),
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

    let mut all_resource_uris = Vec::new();
    let mut cursor = None;
    loop {
        let request = cursor
            .take()
            .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
        let result = client
            .list_resources(request)
            .await
            .expect("list_resources");
        all_resource_uris.extend(result.resources.iter().map(|resource| resource.uri.clone()));
        match result.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(all_resource_uris, vec!["a://1", "a://2", "b://1", "b://2"]);

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn a_uri_listed_by_both_providers_surfaces_as_a_client_visible_error() {
    let call_log = Arc::new(Mutex::new(Vec::new()));
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .resources(
            MergedResourcesProvider::new()
                .with_provider(Stub::new(
                    "a",
                    vec![resource("shared://1")],
                    Vec::new(),
                    call_log.clone(),
                ))
                .with_provider(Stub::new(
                    "b",
                    vec![resource("shared://1")],
                    Vec::new(),
                    call_log.clone(),
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

    let response = client.list_resources(None).await;

    let Err(ServiceError::McpError(error)) = response else {
        panic!("expected an MCP error, got {response:?}");
    };
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert!(error.message.contains("shared://1"), "{error:?}");

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn uri_matching_no_listing_and_no_template_routes_by_scheme_via_real_client() {
    // "c://x" is listed by neither provider and matches no template, but provider "a"
    // is the only one whose other listings use scheme "c" (via "c://known"), so
    // scheme-ownership routing reaches it.
    let call_log = Arc::new(Mutex::new(Vec::new()));
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .resources(
            MergedResourcesProvider::new()
                .with_provider(Stub::new(
                    "a",
                    vec![resource("c://known")],
                    Vec::new(),
                    call_log.clone(),
                ))
                .with_provider(Stub::new(
                    "b",
                    vec![resource("b://1")],
                    Vec::new(),
                    call_log.clone(),
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
        .read_resource(ReadResourceRequestParams::new("c://x"))
        .await
        .expect("read_resource via scheme fallback");
    let rmcp::model::ResourceContents::TextResourceContents { text, .. } = &result.contents[0]
    else {
        panic!("expected text resource contents");
    };
    assert_eq!(text, "read by a");
    assert_eq!(*call_log.lock().expect("call log lock"), vec!["a"]);

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn subscribe_reaches_the_same_provider_read_resource_would_via_real_client() {
    let call_log = Arc::new(Mutex::new(Vec::new()));
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = ServerBuilder::new()
        .info(Implementation::new("test-server", "1.0.0"))
        .resources(
            MergedResourcesProvider::new()
                .with_provider(Stub::new(
                    "a",
                    vec![resource("a://1")],
                    Vec::new(),
                    call_log.clone(),
                ))
                .with_provider(Stub::new(
                    "b",
                    vec![resource("b://1")],
                    Vec::new(),
                    call_log.clone(),
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

    client
        .read_resource(ReadResourceRequestParams::new("b://1"))
        .await
        .expect("read_resource");

    #[allow(
        deprecated,
        reason = "resources/subscribe is legacy-only, still client-reachable"
    )]
    client
        .subscribe(SubscribeRequestParams::new("b://1"))
        .await
        .expect("subscribe");

    assert_eq!(*call_log.lock().expect("call log lock"), vec!["b", "b"]);

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}
