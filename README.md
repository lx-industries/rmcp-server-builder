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
rmcp-server-builder = "0.2"
rmcp = { version = "3.5.1", features = ["server"] }
```

## Merging providers

A server sometimes needs more than one provider of the same capability, for example
tools from two different backends. [`ServerBuilder`] takes one provider per capability,
so compose the several providers into one first, with one of the five merging types:
[`MergedToolsProvider`], [`MergedPromptsProvider`], [`MergedResourcesProvider`],
[`MergedCompletionProvider`], [`MergedLoggingProvider`].

```rust,ignore
use rmcp_server_builder::{MergedResourcesProvider, MergedToolsProvider, ServerBuilder};
use rmcp::model::Implementation;

let tools = MergedToolsProvider::new()
    .with_provider(weather_tools)
    .with_provider(calendar_tools);

let resources = MergedResourcesProvider::new()
    .with_provider(weather_resources)
    .with_provider(calendar_resources);

let server = ServerBuilder::new()
    .info(Implementation::from_build_env())
    .tools(tools)
    .resources(resources)
    .build();
```

Three rules hold across every merging type:

- **Duplicate is an error.** A tool name, a prompt name, or a resource URI listed by
  more than one composed provider is a composition error, not a silent pick of one
  provider over another. A resource template string MAY recur across providers:
  two providers can describe the same URI shape over disjoint data.
- **Cursor composition.** A merged provider's own pagination cursor wraps which
  composed provider listed a page and that provider's own cursor, so a client's
  pagination loop sees one list, drained provider by provider.
- **`listChanged` propagation.** A merged provider caches nothing between calls: it
  re-lists every composed provider on each `list_*` call, so a composed provider's own
  list change is visible on the merged provider's next call, with no separate
  forwarding step.

Each merging type routes a call to the composed provider that owns it:

| Capability | Routes by |
|---|---|
| Tools, prompts | exact name |
| Resources | exact URI, then resource template, then URI scheme |
| Completions | the `ref` the request names (a prompt or a resource template) |
| Logging | fans out: every composed provider receives the call |
| `tasks/*` | the provider that created the task |

## Upgrading from 0.1

0.2 builds on `rmcp` 3.x; 0.1 builds on `rmcp` 1.x. Four signatures change:

| Item | 0.1 | 0.2 |
|---|---|---|
| `ToolsProvider::call_tool` | `Result<CallToolResult, ErrorData>` | `Result<CallToolResponse, ErrorData>` |
| `PromptsProvider::get_prompt` | `Result<GetPromptResult, ErrorData>` | `Result<GetPromptResponse, ErrorData>` |
| `ResourcesProvider::read_resource` | `Result<ReadResourceResult, ErrorData>` | `Result<ReadResourceResponse, ErrorData>` |
| `ServerInfoProvider::get_info` | `ServerInfo` | `ServerConfig` |

Convert an ordinary result with `.into()`:

```rust
Ok(CallToolResult::success(content).into())
```

`ServerInfo` is a deprecated alias of `ServerConfig` in `rmcp` 3.x; rename it to drop the
warning.

Migrate the downstream `ServerHandler` implementations and every boundary that passes `rmcp`
types to `rmcp` 3.x in the same change: the `rmcp` 1.x and 3.x types do not unify.

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

## License

MIT
