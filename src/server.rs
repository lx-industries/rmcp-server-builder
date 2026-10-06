//! The composed Server type and its ServerHandler implementation.

use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CancelledNotificationParam, CompleteRequestParams,
        CompleteResult, ErrorCode, ErrorData, GetPromptRequestParams, GetPromptResponse,
        InitializeRequestParams, InitializeResult, ListPromptsResult, ListResourceTemplatesResult,
        ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProgressNotificationParam,
        ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities, ServerConfig,
        SubscribeRequestParams, UnsubscribeRequestParams,
    },
    service::{NotificationContext, RequestContext, RoleServer},
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

/// Error message for a tools provider that answers `tools/call` with a task.
///
/// The composed server keeps rmcp's `tasks/get`, `tasks/update` and `tasks/cancel` defaults,
/// which answer -32601. A client that received the task handle could never fetch the result.
const TASK_NOT_SERVED_MESSAGE: &str = "the tools provider returned a task, but the composed \
     server does not serve tasks/* yet (rmcp-server-builder#5)";

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
/// - `I`: Server info provider (required)
#[derive(Clone)]
pub struct Server<T, P, R, C, L, I> {
    pub(crate) tools: Option<T>,
    pub(crate) prompts: Option<P>,
    pub(crate) resources: Option<R>,
    pub(crate) completion: Option<C>,
    pub(crate) logging: Option<L>,
    pub(crate) info: I,
    pub(crate) instructions: Option<String>,
}

impl<T, P, R, C, L, I> Server<T, P, R, C, L, I>
where
    I: ServerInfoProvider,
{
    /// Get the capabilities this server advertises.
    ///
    /// Each provider-backed capability (tools, prompts, resources, completions, logging) is
    /// present if and only if its provider is set. For a set provider, the capability keeps the
    /// subflags the info provider configures (`listChanged`, `subscribe`); when the info provider
    /// configures none, the capability is the default. Every other field comes from the info
    /// provider unchanged.
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

        caps
    }
}

// =============================================================================
// ServerHandler implementation
// =============================================================================

impl<T, P, R, C, L, I> ServerHandler for Server<T, P, R, C, L, I>
where
    T: ToolsProvider,
    P: PromptsProvider,
    R: ResourcesProvider,
    C: CompletionProvider,
    L: LoggingProvider,
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

    async fn initialize(
        &self,
        _request: InitializeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let base = self.info.get_info();
        let mut result = InitializeResult::new(self.combined_capabilities())
            .with_protocol_version(base.protocol_version)
            .with_server_info(base.server_info);
        if let Some(instructions) = self.instructions.clone().or(base.instructions) {
            result = result.with_instructions(instructions);
        }
        Ok(result)
    }

    async fn ping(&self, _context: RequestContext<RoleServer>) -> Result<(), ErrorData> {
        Ok(())
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

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match &self.tools {
            Some(provider) => match provider.call_tool(request, context).await {
                Ok(CallToolResponse::Task(_)) => {
                    Err(ErrorData::internal_error(TASK_NOT_SERVED_MESSAGE, None))
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

    async fn on_cancelled(
        &self,
        _notification: CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) {
    }

    async fn on_progress(
        &self,
        _notification: ProgressNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) {
    }

    async fn on_initialized(&self, _context: NotificationContext<RoleServer>) {}

    async fn on_roots_list_changed(&self, _context: NotificationContext<RoleServer>) {}
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

#[cfg(test)]
mod tests {
    use rmcp::model::{
        Implementation, JsonObject, PromptsCapability, ResourcesCapability, ServerCapabilities,
        ToolsCapability,
    };

    use super::{ServerHandler, Unset};
    use crate::{ServerBuilder, SimpleInfo};

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
}
