//! Provider traits for individual MCP capabilities.
//!
//! Each trait represents a single capability group that can be composed into a server.
//! Blanket implementations ensure that any `ServerHandler` automatically implements
//! all provider traits.

use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequestParam, CallToolResult, CompleteRequestParam, CompleteResult, ErrorData,
        GetPromptRequestParam, GetPromptResult, ListPromptsResult, ListResourceTemplatesResult,
        ListResourcesResult, ListToolsResult, PaginatedRequestParam, ReadResourceRequestParam,
        ReadResourceResult, ServerCapabilities, ServerInfo, SetLevelRequestParam,
        SubscribeRequestParam, UnsubscribeRequestParam,
    },
    service::{RequestContext, RoleServer},
};

/// Provider for tools capability.
///
/// Implement this trait to provide tools to a composed server.
pub trait ToolsProvider: Send + Sync + 'static {
    /// List available tools.
    fn list_tools(
        &self,
        request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send;

    /// Execute a tool.
    fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, ErrorData>> + Send;
}

/// Provider for prompts capability.
///
/// Implement this trait to provide prompts to a composed server.
pub trait PromptsProvider: Send + Sync + 'static {
    /// List available prompts.
    fn list_prompts(
        &self,
        request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, ErrorData>> + Send;

    /// Get a specific prompt.
    fn get_prompt(
        &self,
        request: GetPromptRequestParam,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResult, ErrorData>> + Send;
}

/// Provider for resources capability.
///
/// Implement this trait to provide resources to a composed server.
pub trait ResourcesProvider: Send + Sync + 'static {
    /// List available resources.
    fn list_resources(
        &self,
        request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, ErrorData>> + Send;

    /// List resource templates.
    fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + Send;

    /// Read a resource.
    fn read_resource(
        &self,
        request: ReadResourceRequestParam,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResult, ErrorData>> + Send;

    /// Subscribe to resource updates.
    fn subscribe(
        &self,
        request: SubscribeRequestParam,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send;

    /// Unsubscribe from resource updates.
    fn unsubscribe(
        &self,
        request: UnsubscribeRequestParam,
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
        request: CompleteRequestParam,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CompleteResult, ErrorData>> + Send;
}

/// Provider for logging capability.
///
/// Implement this trait to handle logging level changes.
pub trait LoggingProvider: Send + Sync + 'static {
    /// Set the logging level.
    fn set_level(
        &self,
        request: SetLevelRequestParam,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send;
}

/// Provider for server info and initialization.
///
/// This is required for any composed server.
pub trait ServerInfoProvider: Send + Sync + 'static {
    /// Get the server info and capabilities.
    fn get_info(&self) -> ServerInfo;

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
        request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        ServerHandler::list_tools(self, request, context).await
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        ServerHandler::call_tool(self, request, context).await
    }
}

impl<T: ServerHandler> PromptsProvider for T {
    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        ServerHandler::list_prompts(self, request, context).await
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        ServerHandler::get_prompt(self, request, context).await
    }
}

impl<T: ServerHandler> ResourcesProvider for T {
    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        ServerHandler::list_resources(self, request, context).await
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        ServerHandler::list_resource_templates(self, request, context).await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        ServerHandler::read_resource(self, request, context).await
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        ServerHandler::subscribe(self, request, context).await
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        ServerHandler::unsubscribe(self, request, context).await
    }
}

impl<T: ServerHandler> CompletionProvider for T {
    async fn complete(
        &self,
        request: CompleteRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        ServerHandler::complete(self, request, context).await
    }
}

impl<T: ServerHandler> LoggingProvider for T {
    async fn set_level(
        &self,
        request: SetLevelRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        ServerHandler::set_level(self, request, context).await
    }
}

// Note: We intentionally do NOT provide a blanket impl of ServerInfoProvider for ServerHandler
// because it would conflict with our explicit impl for Implementation.
// Users must explicitly implement ServerInfoProvider or use Implementation/SimpleInfo.
