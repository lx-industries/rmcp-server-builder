//! Composable MCP server builder for zero-boilerplate capability composition.
//!
//! This crate provides a builder pattern for composing MCP servers from individual
//! capability providers, eliminating the boilerplate of implementing `ServerHandler`
//! and manually delegating methods.
//!
//! # Overview
//!
//! Instead of implementing the full `ServerHandler` trait and delegating methods,
//! you can compose a server from individual providers:
//!
//! ```ignore
//! use rmcp_server_builder::{Server, ServerBuilder};
//! use rmcp::model::Implementation;
//!
//! // Compose a server with tools from one provider and prompts from another
//! let server = ServerBuilder::new()
//!     .info(Implementation::from_build_env())
//!     .instructions("You have access to a glTF 2.0 API...")
//!     .tools(openapi_server)
//!     .prompts(my_prompts)
//!     .build();
//! ```
//!
//! # Provider Traits
//!
//! Each MCP capability has a corresponding provider trait:
//!
//! - [`ToolsProvider`] - `list_tools`, `call_tool`
//! - [`PromptsProvider`] - `list_prompts`, `get_prompt`
//! - [`ResourcesProvider`] - `list_resources`, `list_resource_templates`, `read_resource`, `subscribe`, `unsubscribe`
//! - [`CompletionProvider`] - `complete`
//! - [`LoggingProvider`] - `set_level`
//! - [`SubscriptionsProvider`] - `subscriptions/listen` (protocol 2026-07-28)
//! - [`ServerInfoProvider`] - `get_info` (required)
//!
//! # Blanket Implementations
//!
//! Any type implementing `ServerHandler` automatically implements every provider trait
//! except [`SubscriptionsProvider`], so existing servers can be used as providers without
//! modification. [`SubscriptionsProvider`] has no blanket implementation, because
//! `ServerHandler::listen` takes a `SubscriptionContext` that only `rmcp` builds.
//!
//! # Merging Providers
//!
//! The builder takes one provider per capability. Compose several providers of one
//! capability with one of the six merging types: [`MergedToolsProvider`],
//! [`MergedPromptsProvider`], [`MergedResourcesProvider`], [`MergedCompletionProvider`],
//! [`MergedLoggingProvider`], [`MergedSubscriptionsProvider`].
//!
//! A merged provider re-lists every composed provider on each `list_*` call, so a list
//! change is visible on the next call. The composed server advertises `listChanged` only
//! when every provider of a [`MergedSubscriptionsProvider`] supports it: one provider's
//! flag would promise notifications the others never send.
//!
//! | Capability | Routes by |
//! |---|---|
//! | Tools, prompts | exact name |
//! | Resources | exact URI, then resource template, then registered scheme owner, then inferred URI scheme |
//! | Completions | the `ref` the request names (a prompt or a resource template) |
//! | Logging | fans out: every composed provider receives the call |
//! | `tasks/*` | the provider that created the task |
//! | Subscriptions | a URI to its one accepting provider; list-changed flags to every provider |
//!
//! # Capability Auto-Detection
//!
//! The composed server automatically sets capability flags based on which providers
//! are configured. If you set a tools provider, `capabilities.tools` will be enabled.

mod builder;
mod merge;
mod providers;
mod server;
mod subscriptions;

pub use builder::{ServerBuilder, SimpleInfo};
pub use merge::{
    MergedCompletionProvider, MergedLoggingProvider, MergedPromptsProvider,
    MergedResourcesProvider, MergedSubscriptionsProvider, MergedToolsProvider,
};
pub use providers::{
    CompletionProvider, LoggingProvider, PromptsProvider, ResourcesProvider, ServerInfoProvider,
    ToolsProvider,
};
pub use server::{Server, Unset};
pub use subscriptions::{
    ListChangedSupport, SubscriptionPart, SubscriptionPartSink, SubscriptionsProvider,
};

// Re-export commonly used rmcp types for convenience
pub use rmcp::handler::server::ServerHandler;
pub use rmcp::model::{Implementation, ServerCapabilities};

impl<T, P, R, C, L, S, I> Server<T, P, R, C, L, S, I> {
    /// Create a new server builder.
    pub fn builder() -> ServerBuilder<Unset, Unset, Unset, Unset, Unset, Unset, Unset> {
        ServerBuilder::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
        GetPromptRequestParams, GetPromptResponse, GetPromptResult, ListPromptsResult,
        ListToolsResult, PaginatedRequestParams,
    };
    use rmcp::service::RequestContext;

    // A simple tools-only provider for testing
    struct TestToolsProvider;

    impl ToolsProvider for TestToolsProvider {
        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<rmcp::service::RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult::with_all_items(vec![]))
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<rmcp::service::RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            Ok(CallToolResult::success(vec![ContentBlock::text("Tool executed")]).into())
        }
    }

    // A simple prompts-only provider for testing
    struct TestPromptsProvider {
        prompt_name: String,
    }

    impl PromptsProvider for TestPromptsProvider {
        async fn list_prompts(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<rmcp::service::RoleServer>,
        ) -> Result<ListPromptsResult, ErrorData> {
            Ok(ListPromptsResult::with_all_items(vec![]))
        }

        async fn get_prompt(
            &self,
            request: GetPromptRequestParams,
            _context: RequestContext<rmcp::service::RoleServer>,
        ) -> Result<GetPromptResponse, ErrorData> {
            if request.name == self.prompt_name {
                Ok(GetPromptResult::new(vec![])
                    .with_description("Test prompt")
                    .into())
            } else {
                Err(ErrorData::invalid_params("Unknown prompt", None))
            }
        }
    }

    #[test]
    fn test_builder_compiles() {
        let _server = ServerBuilder::new()
            .info(Implementation::new("test", "1.0.0"))
            .tools(TestToolsProvider)
            .prompts(TestPromptsProvider {
                prompt_name: "test_prompt".into(),
            })
            .build();
    }

    #[test]
    fn test_server_builder_static_method() {
        let _server = Server::<Unset, Unset, Unset, Unset, Unset, Unset, Unset>::builder()
            .info(Implementation::new("test", "1.0.0"))
            .build();
    }
}
