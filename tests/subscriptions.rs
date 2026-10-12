//! End-to-end tests: an rmcp client holds a `subscriptions/listen` stream on a composed
//! `Server` over a duplex transport, on protocol 2026-07-28.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use rmcp::{
    ClientHandler, ClientLifecycleMode, ClientServiceExt, ServiceExt,
    model::{
        ClientCapabilities, ClientConfig, ClientRequest, CustomNotification, ErrorCode, ErrorData,
        GetMeta, Implementation, ProtocolVersion, RequestMetaObject, ResourcesCapability,
        ServerCapabilities, ServerNotification, SubscriptionFilter, SubscriptionsListenRequest,
        SubscriptionsListenRequestParams, ToolsCapability,
    },
    service::{
        NotificationContext, PeerRequestOptions, RoleClient, RunningService, ServiceError,
        Subscription, SubscriptionEnd, SubscriptionSendError,
    },
};
use rmcp_server_builder::{
    ListChangedSupport, MergedSubscriptionsProvider, ServerBuilder, SimpleInfo, SubscriptionPart,
    SubscriptionsProvider, Unset,
};

/// What a [`Scripted`] provider does once its part starts.
#[derive(Clone)]
#[allow(
    clippy::enum_variant_names,
    reason = "each variant names the update its part sends first"
)]
enum Script {
    /// Sends `notifications/resources/updated` for each accepted URI, then waits for
    /// cancellation.
    UpdateAcceptedThenWait,
    /// Sends `notifications/resources/updated` for each accepted URI, then ends `Ok`.
    UpdateAcceptedThenEnd,
    /// Sends `notifications/resources/updated` for the given URI, records the result,
    /// then waits for cancellation.
    UpdateThenWait(&'static str),
    /// Waits for cancellation, then sends `notifications/resources/updated` for the given
    /// URI and records the result.
    UpdateAfterCancellation(&'static str),
}

/// A subscriptions provider that owns every URI with its prefix and records what its
/// part saw.
struct Scripted {
    prefix: &'static str,
    support: ListChangedSupport,
    script: Script,
    /// `true` once this provider's part observed its cancellation.
    cancelled: Arc<Mutex<bool>>,
    /// The result of the send of [`Script::UpdateThenWait`].
    send_result: Arc<Mutex<Option<Result<(), String>>>>,
}

impl Scripted {
    fn new(prefix: &'static str, script: Script) -> Self {
        Self {
            prefix,
            support: ListChangedSupport::default(),
            script,
            cancelled: Arc::new(Mutex::new(false)),
            send_result: Arc::new(Mutex::new(None)),
        }
    }

    fn with_support(mut self, support: ListChangedSupport) -> Self {
        self.support = support;
        self
    }
}

impl SubscriptionsProvider for Scripted {
    fn support(&self) -> ListChangedSupport {
        self.support
    }

    fn accepts_resource(&self, uri: &str) -> bool {
        uri.starts_with(self.prefix)
    }

    async fn listen(&self, part: SubscriptionPart) -> Result<(), rmcp::ErrorData> {
        match &self.script {
            Script::UpdateAcceptedThenWait => send_accepted_updates(&part).await,
            Script::UpdateAcceptedThenEnd => {
                send_accepted_updates(&part).await;
                return Ok(());
            }
            Script::UpdateThenWait(uri) => {
                let result = part.sink().notify_resource_updated(*uri).await;
                *self.send_result.lock().expect("send result lock") =
                    Some(result.map_err(|error| match error {
                        SubscriptionSendError::NotificationNotAccepted(method) => {
                            format!("not accepted: {method}")
                        }
                        other => other.to_string(),
                    }));
            }
            Script::UpdateAfterCancellation(uri) => {
                part.cancelled().await;
                let result = part.sink().notify_resource_updated(*uri).await;
                *self.send_result.lock().expect("send result lock") =
                    Some(result.map_err(|error| error.to_string()));
                return Ok(());
            }
        }
        part.cancelled().await;
        *self.cancelled.lock().expect("cancelled lock") = true;
        Ok(())
    }
}

/// Sends `notifications/resources/updated` for each URI `part` accepted; stops when
/// another part ended the stream before this part's first send.
async fn send_accepted_updates(part: &SubscriptionPart) {
    let accepted_uris = part
        .accepted()
        .resource_subscriptions
        .clone()
        .unwrap_or_default();
    for uri in accepted_uris {
        match part.sink().notify_resource_updated(uri).await {
            Ok(()) => {}
            Err(SubscriptionSendError::SubscriptionClosed) => break,
            Err(error) => panic!("send an accepted update: {error}"),
        }
    }
}

/// Capabilities with tools and resources, resource subscriptions advertised.
fn info() -> SimpleInfo {
    let mut capabilities = ServerCapabilities::default();
    capabilities.tools = Some(ToolsCapability::default());
    let mut resources = ResourcesCapability::default();
    resources.subscribe = Some(true);
    capabilities.resources = Some(resources);
    SimpleInfo::new(Implementation::new("test-server", "1.0.0")).with_capabilities(capabilities)
}

/// The client configuration of every test: protocol 2026-07-28.
fn client_configuration() -> ClientConfig {
    let mut configuration = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("test-client", "1.0.0"),
    );
    configuration.protocol_version = ProtocolVersion::V_2026_07_28;
    configuration
}

/// Serves `server` on a duplex transport and connects a client on protocol 2026-07-28.
async fn connect<S: rmcp::ServerHandler>(
    server: S,
) -> (
    RunningService<RoleClient, ClientConfig>,
    tokio::task::JoinHandle<()>,
) {
    connect_with(server, client_configuration()).await
}

/// Serves `server` on a duplex transport and connects `client_handler` on protocol
/// 2026-07-28.
async fn connect_with<S: rmcp::ServerHandler, C: ClientHandler>(
    server: S,
    client_handler: C,
) -> (RunningService<RoleClient, C>, tokio::task::JoinHandle<()>) {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });
    let client = client_handler
        .serve_with_lifecycle(
            client_transport,
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("discover server");
    (client, server_task)
}

/// Polls `condition` every 10 ms until it holds; panics after 5 seconds.
async fn eventually(what: &str, condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test]
async fn a_server_without_a_subscriptions_provider_refuses_listen() {
    let server = ServerBuilder::new().info(info()).resources(Unset).build();
    let (client, server_task) = connect(server).await;

    let error = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscription("a://1")
                .build(),
        )
        .await
        .expect_err("listen is method-not-found");

    let rmcp::service::ServiceError::McpError(error) = error else {
        panic!("expected an MCP error, got {error:?}");
    };
    assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND);
    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn one_provider_receives_the_stream_and_its_update_reaches_the_client() {
    let provider = Scripted::new("a://", Script::UpdateAcceptedThenWait);
    let cancelled = Arc::clone(&provider.cancelled);
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(provider)
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions(["a://1", "b://1"])
                .build(),
        )
        .await
        .expect("listen");

    assert_eq!(
        subscription.acknowledged().resource_subscriptions,
        Some(vec!["a://1".to_owned()])
    );
    let notification = subscription
        .next()
        .await
        .expect("next notification")
        .expect("a notification before the stream ends");
    assert_eq!(
        notification.get_meta().subscription_id(),
        Some(subscription.id().clone())
    );
    let ServerNotification::ResourceUpdatedNotification(update) = notification else {
        panic!("expected a resource update, got {notification:?}");
    };
    assert_eq!(update.params.uri, "a://1");

    subscription.cancel().await.expect("cancel subscription");
    eventually("the part to observe its cancellation", || {
        *cancelled.lock().expect("cancelled lock")
    })
    .await;
    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn a_provider_that_returns_ends_the_stream_gracefully() {
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(Scripted::new("a://", Script::UpdateAcceptedThenEnd))
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscription("a://1")
                .build(),
        )
        .await
        .expect("listen");

    let Some(ServerNotification::ResourceUpdatedNotification(update)) =
        subscription.next().await.expect("next notification")
    else {
        panic!("expected a resource update");
    };
    assert_eq!(update.params.uri, "a://1");
    assert!(subscription.next().await.expect("stream end").is_none());
    assert!(
        matches!(subscription.end(), Some(SubscriptionEnd::Graceful(_))),
        "expected a graceful end, got {:?}",
        subscription.end()
    );

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn the_part_sink_refuses_an_update_outside_the_accepted_filter() {
    let provider = Scripted::new("a://", Script::UpdateThenWait("a://2"));
    let send_result = Arc::clone(&provider.send_result);
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(provider)
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscription("a://1")
                .build(),
        )
        .await
        .expect("listen");

    eventually("the provider's send result", || {
        send_result.lock().expect("send result lock").is_some()
    })
    .await;
    assert_eq!(
        *send_result.lock().expect("send result lock"),
        Some(Err(
            "not accepted: notifications/resources/updated".to_owned()
        ))
    );

    subscription.cancel().await.expect("cancel subscription");
    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn the_acknowledgment_keeps_the_list_changed_flags_the_provider_supports() {
    let server = ServerBuilder::new()
        .info(info())
        .tools(Unset)
        .resources(Unset)
        .subscriptions(
            Scripted::new("a://", Script::UpdateAcceptedThenWait).with_support(
                ListChangedSupport {
                    tools: true,
                    prompts: true,
                    resources: false,
                },
            ),
        )
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .tools_list_changed()
                .prompts_list_changed()
                .resources_list_changed()
                .build(),
        )
        .await
        .expect("listen");

    let acknowledged = subscription.acknowledged();
    assert_eq!(acknowledged.tools_list_changed, Some(true));
    assert_eq!(
        acknowledged.prompts_list_changed, None,
        "the server has no prompts capability"
    );
    assert_eq!(
        acknowledged.resources_list_changed, None,
        "the provider does not support it"
    );

    subscription.cancel().await.expect("cancel subscription");
    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

/// A subscriptions provider that owns every URI with its prefix and whose part fails at
/// once.
struct Failing {
    prefix: &'static str,
}

impl SubscriptionsProvider for Failing {
    fn support(&self) -> ListChangedSupport {
        ListChangedSupport::default()
    }

    fn accepts_resource(&self, uri: &str) -> bool {
        uri.starts_with(self.prefix)
    }

    async fn listen(&self, _part: SubscriptionPart) -> Result<(), ErrorData> {
        Err(ErrorData::internal_error("part failed", None))
    }
}

/// Reads `count` notifications and returns the URI of each resource update, sorted.
async fn updated_uris(subscription: &mut Subscription, count: usize) -> Vec<String> {
    let mut uris = Vec::new();
    for _ in 0..count {
        let notification = subscription
            .next()
            .await
            .expect("next notification")
            .expect("a notification before the stream ends");
        assert_eq!(
            notification.get_meta().subscription_id(),
            Some(subscription.id().clone()),
            "the update carries the subscription id"
        );
        let ServerNotification::ResourceUpdatedNotification(update) = notification else {
            panic!("expected a resource update, got {notification:?}");
        };
        uris.push(update.params.uri);
    }
    uris.sort();
    uris
}

#[tokio::test]
async fn each_owner_acknowledges_its_uri_and_its_update_reaches_the_stream() {
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(
            MergedSubscriptionsProvider::new()
                .with_provider(Scripted::new("a://", Script::UpdateAcceptedThenWait))
                .with_provider(Scripted::new("b://", Script::UpdateAcceptedThenWait)),
        )
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions(["a://1", "b://1"])
                .build(),
        )
        .await
        .expect("listen");

    assert_eq!(
        subscription.acknowledged().resource_subscriptions,
        Some(vec!["a://1".to_owned(), "b://1".to_owned()])
    );
    assert_eq!(
        updated_uris(&mut subscription, 2).await,
        vec!["a://1".to_owned(), "b://1".to_owned()]
    );

    subscription.cancel().await.expect("cancel subscription");
    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn an_unowned_and_a_doubly_owned_uri_leave_the_acknowledgment() {
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(
            MergedSubscriptionsProvider::new()
                .with_provider(Scripted::new("a://", Script::UpdateAcceptedThenWait))
                .with_provider(Scripted::new("a://", Script::UpdateAcceptedThenWait))
                .with_provider(Scripted::new("b://", Script::UpdateAcceptedThenWait)),
        )
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions(["a://1", "b://1", "c://1"])
                .build(),
        )
        .await
        .expect("listen");

    assert_eq!(
        subscription.acknowledged().resource_subscriptions,
        Some(vec!["b://1".to_owned()])
    );
    assert_eq!(
        updated_uris(&mut subscription, 1).await,
        vec!["b://1".to_owned()]
    );

    subscription.cancel().await.expect("cancel subscription");
    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

/// Serves two providers with `left` and `right` tools support, requests
/// `toolsListChanged`, and returns the acknowledged flag and the discovered
/// `capabilities.tools.listChanged`.
async fn tools_list_changed_of(left: bool, right: bool) -> (Option<bool>, Option<bool>) {
    let support = |tools| ListChangedSupport {
        tools,
        prompts: false,
        resources: false,
    };
    let server = ServerBuilder::new()
        .info(info())
        .tools(Unset)
        .resources(Unset)
        .subscriptions(
            MergedSubscriptionsProvider::new()
                .with_provider(
                    Scripted::new("a://", Script::UpdateAcceptedThenWait)
                        .with_support(support(left)),
                )
                .with_provider(
                    Scripted::new("b://", Script::UpdateAcceptedThenWait)
                        .with_support(support(right)),
                ),
        )
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(SubscriptionFilter::builder().tools_list_changed().build())
        .await
        .expect("listen");
    let acknowledged = subscription.acknowledged().tools_list_changed;
    let discovered = client
        .discover(RequestMetaObject::default())
        .await
        .expect("discover")
        .capabilities
        .tools
        .expect("tools capability")
        .list_changed;

    subscription.cancel().await.expect("cancel subscription");
    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
    (acknowledged, discovered)
}

#[tokio::test]
async fn tools_list_changed_needs_every_provider() {
    assert_eq!(
        tools_list_changed_of(true, false).await,
        (None, None),
        "one provider lacks the support"
    );
    assert_eq!(
        tools_list_changed_of(true, true).await,
        (Some(true), Some(true)),
        "every provider supports it"
    );
}

#[tokio::test]
async fn when_one_part_ends_the_client_receives_the_completion_and_the_other_part_stops() {
    let waiting = Scripted::new("b://", Script::UpdateAcceptedThenWait);
    let cancelled = Arc::clone(&waiting.cancelled);
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(
            MergedSubscriptionsProvider::new()
                .with_provider(Scripted::new("a://", Script::UpdateAcceptedThenEnd))
                .with_provider(waiting),
        )
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions(["a://1", "b://1"])
                .build(),
        )
        .await
        .expect("listen");

    while subscription
        .next()
        .await
        .expect("next notification")
        .is_some()
    {}
    assert!(
        matches!(subscription.end(), Some(SubscriptionEnd::Graceful(_))),
        "expected a graceful end, got {:?}",
        subscription.end()
    );
    assert!(
        *cancelled.lock().expect("cancelled lock"),
        "the merged listen awaits the other part before it completes"
    );

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn a_part_that_fails_ends_the_stream_with_its_error() {
    let waiting = Scripted::new("b://", Script::UpdateAcceptedThenWait);
    let cancelled = Arc::clone(&waiting.cancelled);
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(
            MergedSubscriptionsProvider::new()
                .with_provider(Failing { prefix: "a://" })
                .with_provider(waiting),
        )
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions(["a://1", "b://1"])
                .build(),
        )
        .await
        .expect("listen");

    let error = loop {
        match subscription.next().await {
            Ok(Some(_)) => {}
            Ok(None) => panic!("expected an error end, got {:?}", subscription.end()),
            Err(error) => break error,
        }
    };
    let ServiceError::McpError(error) = error else {
        panic!("expected an MCP error, got {error:?}");
    };
    assert_eq!(error.message, "part failed");
    assert!(
        matches!(subscription.end(), Some(SubscriptionEnd::Abrupt)),
        "expected an abrupt end, got {:?}",
        subscription.end()
    );
    assert!(
        *cancelled.lock().expect("cancelled lock"),
        "the merged listen awaits the other part before it completes"
    );

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn a_client_cancellation_cancels_every_part() {
    let first = Scripted::new("a://", Script::UpdateAcceptedThenWait);
    let second = Scripted::new("b://", Script::UpdateAcceptedThenWait);
    let first_cancelled = Arc::clone(&first.cancelled);
    let second_cancelled = Arc::clone(&second.cancelled);
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(
            MergedSubscriptionsProvider::new()
                .with_provider(first)
                .with_provider(second),
        )
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions(["a://1", "b://1"])
                .build(),
        )
        .await
        .expect("listen");
    assert_eq!(
        updated_uris(&mut subscription, 2).await,
        vec!["a://1".to_owned(), "b://1".to_owned()]
    );

    subscription.cancel().await.expect("cancel subscription");
    eventually("every part to observe its cancellation", || {
        *first_cancelled.lock().expect("cancelled lock")
            && *second_cancelled.lock().expect("cancelled lock")
    })
    .await;
    // The server answers a later request: with no tools provider, method-not-found.
    let error = client
        .list_tools(None)
        .await
        .expect_err("the server has no tools provider");
    let ServiceError::McpError(error) = error else {
        panic!("expected an answer from the server, got {error:?}");
    };
    assert_eq!(error.code, ErrorCode::METHOD_NOT_FOUND);

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn a_part_sink_refuses_a_uri_of_another_part() {
    let trespassing = Scripted::new("a://", Script::UpdateThenWait("b://1"));
    let send_result = Arc::clone(&trespassing.send_result);
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(
            MergedSubscriptionsProvider::new()
                .with_provider(trespassing)
                .with_provider(Scripted::new("b://", Script::UpdateAcceptedThenWait)),
        )
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions(["a://1", "b://1"])
                .build(),
        )
        .await
        .expect("listen");

    assert_eq!(
        updated_uris(&mut subscription, 1).await,
        vec!["b://1".to_owned()],
        "the update of the part that owns b://1"
    );
    eventually("the provider's send result", || {
        send_result.lock().expect("send result lock").is_some()
    })
    .await;
    assert_eq!(
        *send_result.lock().expect("send result lock"),
        Some(Err(
            "not accepted: notifications/resources/updated".to_owned()
        ))
    );

    subscription.cancel().await.expect("cancel subscription");
    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

/// The `_meta` key the client sets on the listen request.
const PROBE_META_KEY: &str = "example.com/probe";

/// The method of the custom notification a [`PeerProbe`] part sends.
const PROBE_METHOD: &str = "notifications/example/probe";

/// A subscriptions provider whose part records the listen request's `_meta` and sends
/// one custom notification through the peer.
struct PeerProbe {
    /// The `_meta` the part's request context carries.
    meta: Arc<Mutex<Option<RequestMetaObject>>>,
    /// The result of the custom notification's send.
    send_result: Arc<Mutex<Option<Result<(), String>>>>,
}

impl SubscriptionsProvider for PeerProbe {
    fn support(&self) -> ListChangedSupport {
        ListChangedSupport::default()
    }

    fn accepts_resource(&self, _uri: &str) -> bool {
        false
    }

    async fn listen(&self, part: SubscriptionPart) -> Result<(), ErrorData> {
        *self.meta.lock().expect("meta lock") = Some(part.request_context().meta.clone());
        let result = part
            .request_context()
            .peer
            .send_notification(ServerNotification::CustomNotification(
                CustomNotification::new(PROBE_METHOD, None),
            ))
            .await;
        *self.send_result.lock().expect("send result lock") =
            Some(result.map_err(|error| error.to_string()));
        part.cancelled().await;
        Ok(())
    }
}

/// A client handler that records the method of each custom notification.
struct CustomNotificationRecorder {
    methods: Arc<Mutex<Vec<String>>>,
}

impl ClientHandler for CustomNotificationRecorder {
    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _context: NotificationContext<RoleClient>,
    ) {
        self.methods
            .lock()
            .expect("methods lock")
            .push(notification.method);
    }

    fn get_info(&self) -> ClientConfig {
        client_configuration()
    }
}

#[tokio::test]
async fn a_part_receives_the_request_meta_and_the_peer() {
    let probe = PeerProbe {
        meta: Arc::new(Mutex::new(None)),
        send_result: Arc::new(Mutex::new(None)),
    };
    let meta = Arc::clone(&probe.meta);
    let send_result = Arc::clone(&probe.send_result);
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(
            MergedSubscriptionsProvider::new()
                .with_provider(probe)
                .with_provider(Scripted::new("b://", Script::UpdateAcceptedThenWait)),
        )
        .build();
    let methods = Arc::new(Mutex::new(Vec::new()));
    let (client, server_task) = connect_with(
        server,
        CustomNotificationRecorder {
            methods: Arc::clone(&methods),
        },
    )
    .await;

    // `Peer::listen` sets no caller `_meta`; the request API carries one.
    let mut request_meta = RequestMetaObject::default();
    request_meta.insert(
        PROBE_META_KEY.to_owned(),
        rmcp::serde_json::json!("probe value"),
    );
    let request = client
        .send_request_with_option(
            ClientRequest::SubscriptionsListenRequest(SubscriptionsListenRequest::new(
                SubscriptionsListenRequestParams::new(SubscriptionFilter::new()),
            )),
            PeerRequestOptions::no_options().with_meta(request_meta),
        )
        .await
        .expect("send listen");

    eventually("the part's send result", || {
        send_result.lock().expect("send result lock").is_some()
    })
    .await;
    assert_eq!(*send_result.lock().expect("send result lock"), Some(Ok(())));
    assert_eq!(
        meta.lock()
            .expect("meta lock")
            .as_ref()
            .and_then(|meta| meta.get(PROBE_META_KEY).cloned()),
        Some(rmcp::serde_json::json!("probe value"))
    );
    eventually("the client to receive the custom notification", || {
        methods
            .lock()
            .expect("methods lock")
            .iter()
            .any(|method| method == PROBE_METHOD)
    })
    .await;

    request.cancel(None).await.expect("cancel listen");
    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

/// What a [`Handshake`] provider's part does.
enum HandshakeRole {
    /// Ends `Ok` at once.
    EndAtOnce,
    /// After its cancellation, waits for the acknowledgment of the [`HandshakeRole::Acknowledge`] part.
    AwaitAcknowledgment(Mutex<Option<tokio::sync::oneshot::Receiver<()>>>),
    /// After its cancellation, sends the acknowledgment.
    Acknowledge(Mutex<Option<tokio::sync::oneshot::Sender<()>>>),
}

/// A subscriptions provider whose cancelled part's cleanup can depend on another part.
struct Handshake {
    role: HandshakeRole,
}

impl SubscriptionsProvider for Handshake {
    fn support(&self) -> ListChangedSupport {
        ListChangedSupport::default()
    }

    fn accepts_resource(&self, _uri: &str) -> bool {
        false
    }

    async fn listen(&self, part: SubscriptionPart) -> Result<(), ErrorData> {
        match &self.role {
            HandshakeRole::EndAtOnce => Ok(()),
            HandshakeRole::AwaitAcknowledgment(receiver) => {
                part.cancelled().await;
                let receiver = receiver
                    .lock()
                    .expect("receiver lock")
                    .take()
                    .expect("one listen per provider");
                receiver.await.expect("the acknowledgment arrives");
                Ok(())
            }
            HandshakeRole::Acknowledge(sender) => {
                part.cancelled().await;
                let sender = sender
                    .lock()
                    .expect("sender lock")
                    .take()
                    .expect("one listen per provider");
                sender.send(()).expect("the waiting part receives");
                Ok(())
            }
        }
    }
}

#[tokio::test]
async fn a_cancelled_part_whose_cleanup_waits_on_another_cancelled_part_completes() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(
            MergedSubscriptionsProvider::new()
                .with_provider(Handshake {
                    role: HandshakeRole::EndAtOnce,
                })
                .with_provider(Handshake {
                    role: HandshakeRole::AwaitAcknowledgment(Mutex::new(Some(receiver))),
                })
                .with_provider(Handshake {
                    role: HandshakeRole::Acknowledge(Mutex::new(Some(sender))),
                }),
        )
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(SubscriptionFilter::new())
        .await
        .expect("listen");

    let end = tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("the stream ends: every cancelled part is polled until it ends");
    assert!(end.expect("stream end").is_none());
    assert!(
        matches!(subscription.end(), Some(SubscriptionEnd::Graceful(_))),
        "expected a graceful end, got {:?}",
        subscription.end()
    );

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

#[tokio::test]
async fn a_cancelled_part_sink_refuses_to_send() {
    let late = Scripted::new("b://", Script::UpdateAfterCancellation("b://1"));
    let send_result = Arc::clone(&late.send_result);
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(
            MergedSubscriptionsProvider::new()
                .with_provider(Scripted::new("a://", Script::UpdateAcceptedThenEnd))
                .with_provider(late),
        )
        .build();
    let (client, server_task) = connect(server).await;

    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions(["a://1", "b://1"])
                .build(),
        )
        .await
        .expect("listen");
    while subscription
        .next()
        .await
        .expect("next notification")
        .is_some()
    {}

    assert_eq!(
        *send_result.lock().expect("send result lock"),
        Some(Err(SubscriptionSendError::SubscriptionClosed.to_string()))
    );

    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}

/// Holds the server's writes while closed: a write waits until [`WriteGate::open`].
#[derive(Default)]
struct WriteGate {
    closed: std::sync::atomic::AtomicBool,
    /// `true` once a write waited on the closed gate.
    held: std::sync::atomic::AtomicBool,
    waker: Mutex<Option<std::task::Waker>>,
}

impl WriteGate {
    fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn open(&self) {
        self.closed
            .store(false, std::sync::atomic::Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().expect("waker lock").take() {
            waker.wake();
        }
    }

    fn held_a_write(&self) -> bool {
        self.held.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A duplex stream whose writes pass a [`WriteGate`].
struct GatedStream {
    stream: tokio::io::DuplexStream,
    gate: Arc<WriteGate>,
}

impl tokio::io::AsyncRead for GatedStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(context, buffer)
    }
}

impl tokio::io::AsyncWrite for GatedStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.gate.closed.load(std::sync::atomic::Ordering::SeqCst) {
            self.gate
                .held
                .store(true, std::sync::atomic::Ordering::SeqCst);
            *self.gate.waker.lock().expect("waker lock") = Some(context.waker().clone());
            return std::task::Poll::Pending;
        }
        std::pin::Pin::new(&mut self.stream).poll_write(context, buffer)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

/// What a [`Triggered`] provider's part does once its trigger fires.
enum TriggeredRole {
    /// Ends `Ok`.
    End,
    /// Sends `notifications/resources/updated` for the given URI, records the result,
    /// then waits for cancellation.
    Update(&'static str),
}

/// A subscriptions provider that owns every URI with its prefix and acts when the test
/// fires its trigger.
struct Triggered {
    prefix: &'static str,
    trigger: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    role: TriggeredRole,
    send_result: Arc<Mutex<Option<Result<(), String>>>>,
}

impl Triggered {
    fn new(prefix: &'static str, role: TriggeredRole) -> (Self, tokio::sync::oneshot::Sender<()>) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let provider = Self {
            prefix,
            trigger: Mutex::new(Some(receiver)),
            role,
            send_result: Arc::new(Mutex::new(None)),
        };
        (provider, sender)
    }
}

impl SubscriptionsProvider for Triggered {
    fn support(&self) -> ListChangedSupport {
        ListChangedSupport::default()
    }

    fn accepts_resource(&self, uri: &str) -> bool {
        uri.starts_with(self.prefix)
    }

    async fn listen(&self, part: SubscriptionPart) -> Result<(), ErrorData> {
        let trigger = self
            .trigger
            .lock()
            .expect("trigger lock")
            .take()
            .expect("one listen per provider");
        trigger.await.expect("the test fires the trigger");
        match self.role {
            TriggeredRole::End => Ok(()),
            TriggeredRole::Update(uri) => {
                let result = part.sink().notify_resource_updated(uri).await;
                *self.send_result.lock().expect("send result lock") =
                    Some(result.map_err(|error| error.to_string()));
                part.cancelled().await;
                Ok(())
            }
        }
    }
}

#[tokio::test]
async fn a_send_pending_when_its_part_is_cancelled_returns_subscription_closed() {
    let (ending, end) = Triggered::new("a://", TriggeredRole::End);
    let (sending, send) = Triggered::new("b://", TriggeredRole::Update("b://1"));
    let send_result = Arc::clone(&sending.send_result);
    let server = ServerBuilder::new()
        .info(info())
        .resources(Unset)
        .subscriptions(
            MergedSubscriptionsProvider::new()
                .with_provider(ending)
                .with_provider(sending),
        )
        .build();
    let gate = Arc::new(WriteGate::default());
    let (server_stream, client_stream) = tokio::io::duplex(4096);
    let server_transport = GatedStream {
        stream: server_stream,
        gate: Arc::clone(&gate),
    };
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("serve server");
        running.waiting().await.expect("server stops");
    });
    let client = client_configuration()
        .serve_with_lifecycle(
            client_stream,
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("discover server");
    let mut subscription = client
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions(["a://1", "b://1"])
                .build(),
        )
        .await
        .expect("listen");

    gate.close();
    send.send(()).expect("the sending part waits");
    eventually("the update's write to wait on the gate", || {
        gate.held_a_write()
    })
    .await;
    end.send(()).expect("the ending part waits");
    // The pending send returns, with `SubscriptionClosed`. Its notification can still
    // reach the client: the test asserts nothing on that delivery.
    eventually("the pending send to return", || {
        send_result.lock().expect("send result lock").is_some()
    })
    .await;
    assert_eq!(
        *send_result.lock().expect("send result lock"),
        Some(Err(SubscriptionSendError::SubscriptionClosed.to_string()))
    );

    gate.open();
    while subscription
        .next()
        .await
        .expect("next notification")
        .is_some()
    {}
    client.cancel().await.expect("cancel client");
    server_task.await.expect("server task");
}
