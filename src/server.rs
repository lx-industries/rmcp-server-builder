//! The composed Server type and its ServerHandler implementation.

use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, CancelledNotificationParam, CompleteRequestParams,
        CompleteResult, ErrorCode, ErrorData, GetPromptRequestParams, GetPromptResult,
        InitializeRequestParams, InitializeResult, JsonObject, ListPromptsResult,
        ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
        ProgressNotificationParam, PromptsCapability, ReadResourceRequestParams,
        ReadResourceResult, ResourcesCapability, ServerCapabilities, ServerInfo,
        SetLevelRequestParams, SubscribeRequestParams, ToolsCapability, UnsubscribeRequestParams,
    },
    service::{NotificationContext, RequestContext, RoleServer},
};

use crate::providers::{
    CompletionProvider, LoggingProvider, PromptsProvider, ResourcesProvider, ServerInfoProvider,
    ToolsProvider,
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
    /// Get the combined capabilities based on which providers are set.
    fn combined_capabilities(&self) -> ServerCapabilities {
        let mut caps = self.info.capabilities();

        if self.tools.is_some() {
            caps.tools = Some(ToolsCapability::default());
        }
        if self.prompts.is_some() {
            caps.prompts = Some(PromptsCapability::default());
        }
        if self.resources.is_some() {
            caps.resources = Some(ResourcesCapability::default());
        }
        if self.logging.is_some() {
            caps.logging = Some(JsonObject::default());
        }

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
    fn get_info(&self) -> ServerInfo {
        let mut info = self.info.get_info();
        info.capabilities = self.combined_capabilities();
        if self.instructions.is_some() {
            info.instructions = self.instructions.clone();
        }
        info
    }

    async fn initialize(
        &self,
        _request: InitializeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let base = self.info.get_info();
        Ok(InitializeResult {
            protocol_version: base.protocol_version,
            capabilities: self.combined_capabilities(),
            server_info: base.server_info,
            instructions: self.instructions.clone().or(base.instructions),
        })
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
    ) -> Result<CallToolResult, ErrorData> {
        match &self.tools {
            Some(provider) => provider.call_tool(request, context).await,
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
    ) -> Result<GetPromptResult, ErrorData> {
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
    ) -> Result<ReadResourceResult, ErrorData> {
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
    ) -> Result<CallToolResult, ErrorData> {
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
    ) -> Result<GetPromptResult, ErrorData> {
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
    ) -> Result<ReadResourceResult, ErrorData> {
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
