//! Builder for composing MCP servers from individual capability providers.

use rmcp::model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo};

use crate::providers::{
    CompletionProvider, LoggingProvider, PromptsProvider, ResourcesProvider, ServerInfoProvider,
    ToolsProvider,
};
use crate::server::{Server, Unset};

/// Builder for constructing a composed MCP server.
///
/// # Example
///
/// ```ignore
/// use rmcp_server_builder::ServerBuilder;
/// use rmcp::model::Implementation;
///
/// let server = ServerBuilder::new()
///     .info(Implementation::from_build_env())
///     .tools(my_tools_provider)
///     .prompts(my_prompts_provider)
///     .build();
/// ```
pub struct ServerBuilder<T, P, R, C, L, I> {
    tools: Option<T>,
    prompts: Option<P>,
    resources: Option<R>,
    completion: Option<C>,
    logging: Option<L>,
    info: Option<I>,
    instructions: Option<String>,
}

impl Default for ServerBuilder<Unset, Unset, Unset, Unset, Unset, Unset> {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerBuilder<Unset, Unset, Unset, Unset, Unset, Unset> {
    /// Create a new server builder with no providers set.
    pub fn new() -> Self {
        Self {
            tools: None,
            prompts: None,
            resources: None,
            completion: None,
            logging: None,
            info: None,
            instructions: None,
        }
    }
}

impl<T, P, R, C, L, I> ServerBuilder<T, P, R, C, L, I> {
    /// Set the tools provider.
    pub fn tools<NewT: ToolsProvider>(self, provider: NewT) -> ServerBuilder<NewT, P, R, C, L, I> {
        ServerBuilder {
            tools: Some(provider),
            prompts: self.prompts,
            resources: self.resources,
            completion: self.completion,
            logging: self.logging,
            info: self.info,
            instructions: self.instructions,
        }
    }

    /// Set the prompts provider.
    pub fn prompts<NewP: PromptsProvider>(
        self,
        provider: NewP,
    ) -> ServerBuilder<T, NewP, R, C, L, I> {
        ServerBuilder {
            tools: self.tools,
            prompts: Some(provider),
            resources: self.resources,
            completion: self.completion,
            logging: self.logging,
            info: self.info,
            instructions: self.instructions,
        }
    }

    /// Set the resources provider.
    pub fn resources<NewR: ResourcesProvider>(
        self,
        provider: NewR,
    ) -> ServerBuilder<T, P, NewR, C, L, I> {
        ServerBuilder {
            tools: self.tools,
            prompts: self.prompts,
            resources: Some(provider),
            completion: self.completion,
            logging: self.logging,
            info: self.info,
            instructions: self.instructions,
        }
    }

    /// Set the completion provider.
    pub fn completion<NewC: CompletionProvider>(
        self,
        provider: NewC,
    ) -> ServerBuilder<T, P, R, NewC, L, I> {
        ServerBuilder {
            tools: self.tools,
            prompts: self.prompts,
            resources: self.resources,
            completion: Some(provider),
            logging: self.logging,
            info: self.info,
            instructions: self.instructions,
        }
    }

    /// Set the logging provider.
    pub fn logging<NewL: LoggingProvider>(
        self,
        provider: NewL,
    ) -> ServerBuilder<T, P, R, C, NewL, I> {
        ServerBuilder {
            tools: self.tools,
            prompts: self.prompts,
            resources: self.resources,
            completion: self.completion,
            logging: Some(provider),
            info: self.info,
            instructions: self.instructions,
        }
    }

    /// Set the server info provider.
    pub fn info<NewI: ServerInfoProvider>(
        self,
        provider: NewI,
    ) -> ServerBuilder<T, P, R, C, L, NewI> {
        ServerBuilder {
            tools: self.tools,
            prompts: self.prompts,
            resources: self.resources,
            completion: self.completion,
            logging: self.logging,
            info: Some(provider),
            instructions: self.instructions,
        }
    }

    /// Set human-readable instructions for using this server.
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }
}

// Build method - requires I to be set
impl<T, P, R, C, L, I> ServerBuilder<T, P, R, C, L, I>
where
    I: ServerInfoProvider,
{
    /// Build the server.
    ///
    /// This will use `Unset` for any providers that weren't explicitly set,
    /// which will return "method not found" errors for those capabilities.
    pub fn build(self) -> Server<T, P, R, C, L, I> {
        Server {
            tools: self.tools,
            prompts: self.prompts,
            resources: self.resources,
            completion: self.completion,
            logging: self.logging,
            info: self.info.expect("info provider is required"),
            instructions: self.instructions,
        }
    }
}

// =============================================================================
// ServerInfoProvider implementation for Implementation
// =============================================================================

impl ServerInfoProvider for Implementation {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::default(),
            server_info: self.clone(),
            instructions: None,
        }
    }
}

/// A simple server info provider that wraps an Implementation.
#[derive(Clone, Debug)]
pub struct SimpleInfo {
    pub server_info: Implementation,
    pub capabilities: ServerCapabilities,
}

impl SimpleInfo {
    /// Create a new SimpleInfo from an Implementation.
    pub fn new(server_info: Implementation) -> Self {
        Self {
            server_info,
            capabilities: ServerCapabilities::default(),
        }
    }

    /// Set capabilities.
    pub fn with_capabilities(mut self, capabilities: ServerCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

impl ServerInfoProvider for SimpleInfo {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: self.capabilities.clone(),
            server_info: self.server_info.clone(),
            instructions: None,
        }
    }

    fn capabilities(&self) -> ServerCapabilities {
        self.capabilities.clone()
    }
}
