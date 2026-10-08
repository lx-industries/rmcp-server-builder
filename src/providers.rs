//! Provider traits for individual MCP capabilities.
//!
//! Each trait represents a single capability group that can be composed into a server.
//! Blanket implementations ensure that any `ServerHandler` automatically implements
//! all provider traits.

use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, CompleteRequestParams,
        CompleteResult, ErrorCode, ErrorData, GetPromptRequestParams, GetPromptResponse,
        GetTaskParams, GetTaskResult, ListPromptsResult, ListResourceTemplatesResult,
        ListResourcesResult, ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams,
        ReadResourceResponse, ServerCapabilities, ServerConfig, SubscribeRequestParams,
        UnsubscribeRequestParams, UpdateTaskParams,
    },
    service::{RequestContext, RoleServer},
};

#[expect(
    deprecated,
    reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
)]
use rmcp::model::SetLevelRequestParams;

/// Provider for tools capability.
///
/// Implement this trait to provide tools to a composed server.
pub trait ToolsProvider: Send + Sync + 'static {
    /// List available tools.
    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send;

    /// Execute a tool.
    ///
    /// The composed [`Server`](crate::Server) answers a [`CallToolResponse::Task`] with an
    /// internal error, because it does not serve `tasks/*` yet.
    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send;

    /// Get the current state of a task created by [`ToolsProvider::call_tool`].
    ///
    /// The default implementation answers `METHOD_NOT_FOUND`: override it to serve
    /// `tasks/get` (SEP-2663) for a provider whose `call_tool` answers
    /// [`CallToolResponse::Task`].
    fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetTaskResult, ErrorData>> + Send {
        async move {
            let _ = (request, context);
            Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "tasks not supported",
                None,
            ))
        }
    }

    /// Deliver responses to a task's outstanding in-task input requests.
    ///
    /// The default implementation answers `METHOD_NOT_FOUND`: override it to serve
    /// `tasks/update` (SEP-2663) for a provider whose `call_tool` answers
    /// [`CallToolResponse::Task`].
    fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send {
        async move {
            let _ = (request, context);
            Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "tasks not supported",
                None,
            ))
        }
    }

    /// Request cooperative cancellation of a task created by [`ToolsProvider::call_tool`].
    ///
    /// The default implementation answers `METHOD_NOT_FOUND`: override it to serve
    /// `tasks/cancel` (SEP-2663) for a provider whose `call_tool` answers
    /// [`CallToolResponse::Task`].
    fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send {
        async move {
            let _ = (request, context);
            Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                "tasks not supported",
                None,
            ))
        }
    }
}

/// Provider for prompts capability.
///
/// Implement this trait to provide prompts to a composed server.
pub trait PromptsProvider: Send + Sync + 'static {
    /// List available prompts.
    fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, ErrorData>> + Send;

    /// Get a specific prompt.
    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResponse, ErrorData>> + Send;
}

/// Provider for resources capability.
///
/// Implement this trait to provide resources to a composed server.
pub trait ResourcesProvider: Send + Sync + 'static {
    /// List available resources.
    fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, ErrorData>> + Send;

    /// List resource templates.
    fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + Send;

    /// Read a resource.
    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, ErrorData>> + Send;

    /// Subscribe to resource updates.
    fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send;

    /// Unsubscribe from resource updates.
    fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send;
}

/// Provider for completion capability.
///
/// Implement this trait to provide completion suggestions to a composed server.
pub trait CompletionProvider: Send + Sync + 'static {
    /// Provide completion suggestions.
    fn complete(
        &self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CompleteResult, ErrorData>> + Send;
}

/// Provider for logging capability.
///
/// Implement this trait to handle logging level changes.
pub trait LoggingProvider: Send + Sync + 'static {
    /// Set the logging level.
    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    fn set_level(
        &self,
        request: SetLevelRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send;
}

/// Provider for server info and initialization.
///
/// This is required for any composed server.
pub trait ServerInfoProvider: Send + Sync + 'static {
    /// Get the server info and capabilities.
    fn get_info(&self) -> ServerConfig;

    /// Get the base capabilities (before provider-based adjustments).
    fn capabilities(&self) -> ServerCapabilities {
        self.get_info().capabilities.clone()
    }
}

// =============================================================================
// Blanket implementations: ServerHandler -> Provider traits
// =============================================================================

impl<T: ServerHandler> ToolsProvider for T {
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        ServerHandler::list_tools(self, request, context).await
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        ServerHandler::call_tool(self, request, context).await
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        ServerHandler::get_task(self, request, context).await
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        ServerHandler::update_task(self, request, context).await
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        ServerHandler::cancel_task(self, request, context).await
    }
}

impl<T: ServerHandler> PromptsProvider for T {
    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        ServerHandler::list_prompts(self, request, context).await
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        ServerHandler::get_prompt(self, request, context).await
    }
}

impl<T: ServerHandler> ResourcesProvider for T {
    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        ServerHandler::list_resources(self, request, context).await
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        ServerHandler::list_resource_templates(self, request, context).await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        ServerHandler::read_resource(self, request, context).await
    }

    #[expect(
        deprecated,
        reason = "rmcp 3.x keeps resources/subscribe and resources/unsubscribe for legacy protocol versions"
    )]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        ServerHandler::subscribe(self, request, context).await
    }

    #[expect(
        deprecated,
        reason = "rmcp 3.x keeps resources/subscribe and resources/unsubscribe for legacy protocol versions"
    )]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        ServerHandler::unsubscribe(self, request, context).await
    }
}

impl<T: ServerHandler> CompletionProvider for T {
    async fn complete(
        &self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        ServerHandler::complete(self, request, context).await
    }
}

impl<T: ServerHandler> LoggingProvider for T {
    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    async fn set_level(
        &self,
        request: SetLevelRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        ServerHandler::set_level(self, request, context).await
    }
}

// Note: We intentionally do NOT provide a blanket impl of ServerInfoProvider for ServerHandler
// because it would conflict with our explicit impl for Implementation.
// Users must explicitly implement ServerInfoProvider or use Implementation/SimpleInfo.
