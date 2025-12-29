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
//! - [`ServerInfoProvider`] - `get_info` (required)
//!
//! # Blanket Implementations
//!
//! Any type implementing `ServerHandler` automatically implements all provider traits,
//! so existing servers can be used as providers without modification.
//!
//! # Capability Auto-Detection
//!
//! The composed server automatically sets capability flags based on which providers
//! are configured. If you set a tools provider, `capabilities.tools` will be enabled.

mod builder;
mod providers;
mod server;

pub use builder::{ServerBuilder, SimpleInfo};
pub use providers::{
    CompletionProvider, LoggingProvider, PromptsProvider, ResourcesProvider, ServerInfoProvider,
    ToolsProvider,
};
pub use server::{Server, Unset};

// Re-export commonly used rmcp types for convenience
pub use rmcp::handler::server::ServerHandler;
pub use rmcp::model::{Implementation, ServerCapabilities};

impl<T, P, R, C, L, I> Server<T, P, R, C, L, I> {
    /// Create a new server builder.
    pub fn builder() -> ServerBuilder<Unset, Unset, Unset, Unset, Unset, Unset> {
        ServerBuilder::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{
        CallToolRequestParam, ErrorData, GetPromptRequestParam, GetPromptResult, ListPromptsResult,
        ListToolsResult, PaginatedRequestParam,
    };
    use rmcp::service::RequestContext;

    // A simple tools-only provider for testing
    struct TestToolsProvider;

    impl ToolsProvider for TestToolsProvider {
        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParam>,
            _context: RequestContext<rmcp::service::RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            Ok(ListToolsResult::with_all_items(vec![]))
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParam,
            _context: RequestContext<rmcp::service::RoleServer>,
        ) -> Result<rmcp::model::CallToolResult, ErrorData> {
            Ok(rmcp::model::CallToolResult::success(vec![
                rmcp::model::Content::text("Tool executed"),
            ]))
        }
    }

    // A simple prompts-only provider for testing
    struct TestPromptsProvider {
        prompt_name: String,
    }

    impl PromptsProvider for TestPromptsProvider {
        async fn list_prompts(
            &self,
            _request: Option<PaginatedRequestParam>,
            _context: RequestContext<rmcp::service::RoleServer>,
        ) -> Result<ListPromptsResult, ErrorData> {
            Ok(ListPromptsResult::with_all_items(vec![]))
        }

        async fn get_prompt(
            &self,
            request: GetPromptRequestParam,
            _context: RequestContext<rmcp::service::RoleServer>,
        ) -> Result<GetPromptResult, ErrorData> {
            if request.name == self.prompt_name {
                Ok(GetPromptResult {
                    description: Some("Test prompt".into()),
                    messages: vec![],
                })
            } else {
                Err(ErrorData::invalid_params("Unknown prompt", None))
            }
        }
    }

    #[test]
    fn test_builder_compiles() {
        let _server = ServerBuilder::new()
            .info(Implementation {
                name: "test".into(),
                version: "1.0.0".into(),
                ..Default::default()
            })
            .tools(TestToolsProvider)
            .prompts(TestPromptsProvider {
                prompt_name: "test_prompt".into(),
            })
            .build();
    }

    #[test]
    fn test_server_builder_static_method() {
        let _server = Server::<Unset, Unset, Unset, Unset, Unset, Unset>::builder()
            .info(Implementation {
                name: "test".into(),
                version: "1.0.0".into(),
                ..Default::default()
            })
            .build();
    }
}
