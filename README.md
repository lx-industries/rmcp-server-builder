# rmcp-server-builder

Composable MCP server builder for zero-boilerplate capability composition.

## Overview

This crate provides a builder pattern for composing MCP servers from individual capability providers, eliminating the boilerplate of implementing `ServerHandler` and manually delegating methods.

Instead of implementing the full `ServerHandler` trait:

```rust
// Traditional approach - lots of boilerplate
impl ServerHandler for MyServer {
    fn list_tools(&self, ...) -> ... { self.tools.list_tools(...) }
    fn call_tool(&self, ...) -> ... { self.tools.call_tool(...) }
    fn list_prompts(&self, ...) -> ... { self.prompts.list_prompts(...) }
    fn get_prompt(&self, ...) -> ... { self.prompts.get_prompt(...) }
    // ... many more delegations
}
```

You can compose a server from individual providers:

```rust
use rmcp_server_builder::ServerBuilder;
use rmcp::model::Implementation;

let server = ServerBuilder::new()
    .info(Implementation::from_build_env())
    .instructions("A helpful assistant with access to various tools.")
    .tools(my_tools_provider)
    .prompts(my_prompts_provider)
    .build();
```

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
rmcp-server-builder = "0.1"
rmcp = { version = "0.12", features = ["server"] }
```

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

## License

MIT
