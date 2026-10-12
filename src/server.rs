//! The composed Server type and its ServerHandler implementation.

use std::borrow::Cow;

use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, CompleteRequestParams,
        CompleteResult, ErrorCode, ErrorData, GetPromptRequestParams, GetPromptResponse,
        GetTaskParams, GetTaskResult, ListPromptsResult, ListResourceTemplatesResult,
        ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
        ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities, ServerConfig,
        SubscribeRequestParams, SubscriptionFilter, UnsubscribeRequestParams, UpdateTaskParams,
    },
    service::{RequestContext, RoleServer, SubscriptionContext},
};

#[expect(
    deprecated,
    reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
)]
use rmcp::model::SetLevelRequestParams;

use crate::providers::{
    CompletionProvider, LoggingProvider, PromptsProvider, ResourcesProvider, ServerInfoProvider,
    ToolsProvider,
};
use crate::subscriptions::{
    ListChangedSupport, SubscriptionPart, SubscriptionsProvider, accepted_filter,
};

/// Marker for an unset provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unset;

/// A composable MCP server that routes requests to individual capability providers.
///
/// Use [`ServerBuilder`](crate::ServerBuilder) to construct a server.
///
/// # Type Parameters
///
/// - `T`: Tools provider (or `Unset`)
/// - `P`: Prompts provider (or `Unset`)
/// - `R`: Resources provider (or `Unset`)
/// - `C`: Completion provider (or `Unset`)
/// - `L`: Logging provider (or `Unset`)
/// - `S`: Subscriptions provider (or `Unset`)
/// - `I`: Server info provider (required)
#[derive(Clone)]
pub struct Server<T, P, R, C, L, S, I> {
    pub(crate) tools: Option<T>,
    pub(crate) prompts: Option<P>,
    pub(crate) resources: Option<R>,
    pub(crate) completion: Option<C>,
    pub(crate) logging: Option<L>,
    pub(crate) subscriptions: Option<S>,
    pub(crate) info: I,
    pub(crate) instructions: Option<String>,
}

impl<T, P, R, C, L, S, I> Server<T, P, R, C, L, S, I>
where
    S: SubscriptionsProvider,
    I: ServerInfoProvider,
{
    /// Get the capabilities this server advertises.
    ///
    /// Each provider-backed capability (tools, prompts, resources, completions, logging) is
    /// present if and only if its provider is set. For a set provider, the capability keeps the
    /// subflags the info provider configures (`listChanged`, `subscribe`); when the info provider
    /// configures none, the capability is the default. Every other field comes from the info
    /// provider unchanged.
    ///
    /// With a subscriptions provider set, `listChanged` on tools, prompts and resources comes
    /// from the provider's [`support`](SubscriptionsProvider::support) instead.
    /// `resources.subscribe` stays with the info provider: `rmcp` drops requested resource
    /// subscriptions from a `subscriptions/listen` acknowledgment unless it is advertised.
    fn combined_capabilities(&self) -> ServerCapabilities {
        let mut caps = self.info.capabilities();

        caps.tools = self
            .tools
            .as_ref()
            .map(|_| caps.tools.take().unwrap_or_default());
        caps.prompts = self
            .prompts
            .as_ref()
            .map(|_| caps.prompts.take().unwrap_or_default());
        caps.resources = self
            .resources
            .as_ref()
            .map(|_| caps.resources.take().unwrap_or_default());
        caps.completions = self
            .completion
            .as_ref()
            .map(|_| caps.completions.take().unwrap_or_default());
        caps.logging = self
            .logging
            .as_ref()
            .map(|_| caps.logging.take().unwrap_or_default());

        if let Some(provider) = &self.subscriptions {
            let support = provider.support();
            if let Some(tools) = caps.tools.as_mut() {
                tools.list_changed = support.tools.then_some(true);
            }
            if let Some(prompts) = caps.prompts.as_mut() {
                prompts.list_changed = support.prompts.then_some(true);
            }
            if let Some(resources) = caps.resources.as_mut() {
                resources.list_changed = support.resources.then_some(true);
            }
        }

        caps
    }
}

// =============================================================================
// ServerHandler implementation
// =============================================================================

impl<T, P, R, C, L, S, I> ServerHandler for Server<T, P, R, C, L, S, I>
where
    T: ToolsProvider,
    P: PromptsProvider,
    R: ResourcesProvider,
    C: CompletionProvider,
    L: LoggingProvider,
    S: SubscriptionsProvider,
    I: ServerInfoProvider,
{
    fn get_info(&self) -> ServerConfig {
        let base = self.info.get_info();
        let mut info = ServerConfig::new(self.combined_capabilities())
            .with_protocol_version(base.protocol_version)
            .with_server_info(base.server_info);
        if let Some(instructions) = self.instructions.clone().or(base.instructions) {
            info = info.with_instructions(instructions);
        }
        info
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        self.info.supported_protocol_versions()
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        match &self.tools {
            Some(provider) => provider.list_tools(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "tools not supported",
                None,
            )),
        }
    }

    /// Executes a tool through the composed tools provider.
    ///
    /// Forwards a [`CallToolResponse::Task`] as-is only when the server's combined
    /// capabilities advertise the `io.modelcontextprotocol/tasks` extension
    /// (`supports_tasks()`): only then does the composed [`ServerHandler`] serve
    /// `tasks/get`, `tasks/update` and `tasks/cancel` for it (see this impl's
    /// `get_task`/`update_task`/`cancel_task`, which delegate to `self.tools`).
    /// Otherwise answers `ErrorData::internal_error`, naming that the tools provider
    /// returned a task while the server does not advertise the tasks extension, and
    /// how to enable it.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match &self.tools {
            Some(provider) => match provider.call_tool(request, context).await {
                Ok(CallToolResponse::Task(created))
                    if !self.combined_capabilities().supports_tasks() =>
                {
                    Err(ErrorData::internal_error(
                        format!(
                            "the tools provider returned a task (id {task_id:?}), but the \
                             composed server does not advertise the tasks extension; \
                             enable it with \
                             ServerCapabilities::builder().enable_tasks() on the info provider",
                            task_id = created.task.task_id
                        ),
                        None,
                    ))
                }
                response => response,
            },
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "tools not supported",
                None,
            )),
        }
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        match &self.tools {
            Some(provider) => provider.get_task(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "tools not supported",
                None,
            )),
        }
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        match &self.tools {
            Some(provider) => provider.update_task(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "tools not supported",
                None,
            )),
        }
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        match &self.tools {
            Some(provider) => provider.cancel_task(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "tools not supported",
                None,
            )),
        }
    }

    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        match &self.prompts {
            Some(provider) => provider.list_prompts(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "prompts not supported",
                None,
            )),
        }
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        match &self.prompts {
            Some(provider) => provider.get_prompt(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "prompts not supported",
                None,
            )),
        }
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        match &self.resources {
            Some(provider) => provider.list_resources(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "resources not supported",
                None,
            )),
        }
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        match &self.resources {
            Some(provider) => provider.list_resource_templates(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "resources not supported",
                None,
            )),
        }
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        match &self.resources {
            Some(provider) => provider.read_resource(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "resources not supported",
                None,
            )),
        }
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        match &self.resources {
            Some(provider) => provider.subscribe(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "resources not supported",
                None,
            )),
        }
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        match &self.resources {
            Some(provider) => provider.unsubscribe(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "resources not supported",
                None,
            )),
        }
    }

    async fn complete(
        &self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        match &self.completion {
            Some(provider) => provider.complete(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "completion not supported",
                None,
            )),
        }
    }

    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    async fn set_level(
        &self,
        request: SetLevelRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        match &self.logging {
            Some(provider) => provider.set_level(request, context).await,
            None => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "logging not supported",
                None,
            )),
        }
    }

    /// Acknowledges, with a subscriptions provider set, each requested list-changed flag
    /// the provider supports and each requested URI it accepts. With none set, answers
    /// `None`: `subscriptions/listen` is method-not-found.
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        self.subscriptions.as_ref().map(|provider| {
            accepted_filter(
                provider.support(),
                |uri| provider.accepts_resource(uri),
                requested,
            )
        })
    }

    /// Runs the subscriptions provider on the whole acknowledged stream. The
    /// `SubscriptionContext` lives until the provider returns: dropping it closes every
    /// sink of the stream.
    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        match &self.subscriptions {
            Some(provider) => {
                provider
                    .listen(SubscriptionPart::from_context(&context))
                    .await
            }
            None => {
                context.cancelled().await;
                Ok(())
            }
        }
    }
}

// =============================================================================
// Provider implementations for Unset marker
// =============================================================================

impl ToolsProvider for Unset {
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "tools not supported",
            None,
        ))
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "tools not supported",
            None,
        ))
    }

    async fn get_task(
        &self,
        _request: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "tools not supported",
            None,
        ))
    }

    async fn update_task(
        &self,
        _request: UpdateTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "tools not supported",
            None,
        ))
    }

    async fn cancel_task(
        &self,
        _request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "tools not supported",
            None,
        ))
    }
}

impl PromptsProvider for Unset {
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "prompts not supported",
            None,
        ))
    }

    async fn get_prompt(
        &self,
        _request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "prompts not supported",
            None,
        ))
    }
}

impl ResourcesProvider for Unset {
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "resources not supported",
            None,
        ))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "resources not supported",
            None,
        ))
    }

    async fn read_resource(
        &self,
        _request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "resources not supported",
            None,
        ))
    }

    async fn subscribe(
        &self,
        _request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "resources not supported",
            None,
        ))
    }

    async fn unsubscribe(
        &self,
        _request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "resources not supported",
            None,
        ))
    }
}

impl CompletionProvider for Unset {
    async fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "completion not supported",
            None,
        ))
    }
}

impl LoggingProvider for Unset {
    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    async fn set_level(
        &self,
        _request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "logging not supported",
            None,
        ))
    }
}

impl SubscriptionsProvider for Unset {
    fn support(&self) -> ListChangedSupport {
        ListChangedSupport::default()
    }

    fn accepts_resource(&self, _uri: &str) -> bool {
        false
    }

    async fn listen(&self, part: SubscriptionPart) -> Result<(), ErrorData> {
        part.cancelled().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use rmcp::model::{
        ErrorData, Implementation, JsonObject, PromptsCapability, ProtocolVersion,
        ResourcesCapability, ServerCapabilities, ServerConfig, ToolsCapability,
    };

    use super::{ServerHandler, Unset};
    use crate::{
        ListChangedSupport, MergedToolsProvider, ServerBuilder, SimpleInfo, SubscriptionPart,
        SubscriptionsProvider,
    };

    /// An info provider that implements only protocol version 2026-07-28.
    struct LatestOnlyInfo;

    impl crate::ServerInfoProvider for LatestOnlyInfo {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(ServerCapabilities::default())
                .with_server_info(Implementation::new("test", "1.0.0"))
        }

        fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
            Cow::Owned(vec![ProtocolVersion::V_2026_07_28])
        }
    }

    #[test]
    fn the_server_supports_the_protocol_versions_of_the_info_provider() {
        let server = ServerBuilder::new().info(LatestOnlyInfo).build();

        assert_eq!(
            ServerHandler::supported_protocol_versions(&server).as_ref(),
            [ProtocolVersion::V_2026_07_28]
        );
    }

    #[test]
    fn the_default_info_provider_supports_every_known_version() {
        let server = ServerBuilder::new()
            .info(Implementation::new("test", "1.0.0"))
            .build();

        assert_eq!(
            ServerHandler::supported_protocol_versions(&server).as_ref(),
            ProtocolVersion::KNOWN_VERSIONS
        );
    }

    /// Capabilities that advertise every provider-backed capability, with every subflag set.
    fn advertised_capabilities() -> ServerCapabilities {
        let mut capabilities = ServerCapabilities::default();
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(true);
        capabilities.tools = Some(tools);
        let mut prompts = PromptsCapability::default();
        prompts.list_changed = Some(true);
        capabilities.prompts = Some(prompts);
        let mut resources = ResourcesCapability::default();
        resources.subscribe = Some(true);
        resources.list_changed = Some(true);
        capabilities.resources = Some(resources);
        let mut flag = JsonObject::new();
        flag.insert("configured".into(), true.into());
        capabilities.logging = Some(flag.clone());
        capabilities.completions = Some(flag);
        capabilities
    }

    fn info(capabilities: ServerCapabilities) -> SimpleInfo {
        SimpleInfo::new(Implementation::new("test", "1.0.0")).with_capabilities(capabilities)
    }

    #[test]
    fn each_installed_provider_advertises_its_capability() {
        let server = ServerBuilder::new()
            .info(Implementation::new("test", "1.0.0"))
            .tools(Unset)
            .prompts(Unset)
            .resources(Unset)
            .completion(Unset)
            .logging(Unset)
            .build();

        let capabilities = server.get_info().capabilities;

        assert!(capabilities.tools.is_some());
        assert!(capabilities.prompts.is_some());
        assert!(capabilities.resources.is_some());
        assert!(capabilities.completions.is_some());
        assert!(capabilities.logging.is_some());
    }

    #[test]
    fn an_absent_provider_clears_the_capability_the_info_provider_advertises() {
        let server = ServerBuilder::new()
            .info(info(advertised_capabilities()))
            .build();

        let capabilities = server.get_info().capabilities;

        assert!(capabilities.tools.is_none());
        assert!(capabilities.prompts.is_none());
        assert!(capabilities.resources.is_none());
        assert!(capabilities.completions.is_none());
        assert!(capabilities.logging.is_none());
    }

    #[test]
    fn an_installed_provider_keeps_the_subflags_the_info_provider_configures() {
        let server = ServerBuilder::new()
            .info(info(advertised_capabilities()))
            .tools(Unset)
            .prompts(Unset)
            .resources(Unset)
            .completion(Unset)
            .logging(Unset)
            .build();

        let capabilities = server.get_info().capabilities;

        assert_eq!(capabilities, advertised_capabilities());
    }

    /// A merged tools provider is a tools provider like any other: the subflag it
    /// advertises comes from the info provider's base capabilities, not from the
    /// provider slot's concrete type (see `combined_capabilities`).
    #[test]
    fn a_merged_tools_provider_keeps_the_subflags_the_info_provider_configures() {
        let server = ServerBuilder::new()
            .info(info(advertised_capabilities()))
            .tools(MergedToolsProvider::new())
            .build();

        let capabilities = server.get_info().capabilities;

        assert_eq!(capabilities.tools, advertised_capabilities().tools);
    }

    /// A subscriptions provider that supports the list-changed flags it holds, accepts no
    /// resource, and waits for cancellation.
    struct StaticSupport(ListChangedSupport);

    impl SubscriptionsProvider for StaticSupport {
        fn support(&self) -> ListChangedSupport {
            self.0
        }

        fn accepts_resource(&self, _uri: &str) -> bool {
            false
        }

        async fn listen(&self, part: SubscriptionPart) -> Result<(), ErrorData> {
            part.cancelled().await;
            Ok(())
        }
    }

    #[test]
    fn a_subscriptions_provider_sets_list_changed_from_its_support() {
        let server = ServerBuilder::new()
            .info(info(advertised_capabilities()))
            .tools(Unset)
            .prompts(Unset)
            .resources(Unset)
            .subscriptions(StaticSupport(ListChangedSupport {
                tools: true,
                prompts: false,
                resources: true,
            }))
            .build();

        let capabilities = server.get_info().capabilities;

        let tools = capabilities.tools.expect("tools capability");
        let prompts = capabilities.prompts.expect("prompts capability");
        let resources = capabilities.resources.expect("resources capability");
        assert_eq!(tools.list_changed, Some(true));
        assert_eq!(prompts.list_changed, None);
        assert_eq!(resources.list_changed, Some(true));
        assert_eq!(resources.subscribe, Some(true));
    }
}
