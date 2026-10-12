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
so compose the several providers into one first, with one of the six merging types:
[`MergedToolsProvider`], [`MergedPromptsProvider`], [`MergedResourcesProvider`],
[`MergedCompletionProvider`], [`MergedLoggingProvider`], [`MergedSubscriptionsProvider`].

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
  provider over another. `list_*` fails naming the duplicated key and the two
  providers that list it. A call of a tool name fails only when that name itself is
  duplicated; a call of any other, unique tool name still routes. Known limitation: a
  call drains every provider's `list_tools` first, so a failing `list_tools` of any one
  provider fails every tool call. A resource
  template string MAY recur across providers: two providers can describe the same
  URI shape over disjoint data.
- **Cursor composition.** A merged provider's own pagination cursor wraps which
  composed provider listed a page and that provider's own cursor, so a client's
  pagination loop sees one list, drained provider by provider.
- **`listChanged` propagation.** A merged provider re-lists every composed provider on
  each `list_*` call, so a list change is visible on the next call. The composed server
  advertises `listChanged` only when every provider of a `MergedSubscriptionsProvider`
  supports it: one provider's flag would promise notifications the others never send.

Each merging type routes a call to the composed provider that owns it:

| Capability | Routes by |
|---|---|
| Tools, prompts | exact name |
| Resources | exact URI, then resource template, then registered scheme owner, then inferred URI scheme |
| Completions | the `ref` the request names (a prompt or a resource template) |
| Logging | fans out: every composed provider receives the call |
| Subscriptions | a URI to its one accepting provider; list-changed flags to every provider |
| `tasks/*` | the provider that created the task |

## Subscriptions

Protocol 2026-07-28 delivers change notifications on one `subscriptions/listen` stream.
Implement [`SubscriptionsProvider`] and set it with `ServerBuilder::subscriptions`:

```rust,ignore
use rmcp_server_builder::{
    ListChangedSupport, ServerBuilder, SubscriptionPart, SubscriptionsProvider,
};

struct Watcher;

impl SubscriptionsProvider for Watcher {
    fn support(&self) -> ListChangedSupport {
        ListChangedSupport { tools: true, prompts: false, resources: false }
    }

    fn accepts_resource(&self, uri: &str) -> bool {
        uri.starts_with("file://")
    }

    async fn listen(&self, part: SubscriptionPart) -> Result<(), rmcp::ErrorData> {
        part.sink().notify_tool_list_changed().await.ok();
        part.cancelled().await;
        Ok(())
    }
}

let server = ServerBuilder::new()
    .info(info)
    .tools(tools)
    .resources(resources)
    .subscriptions(Watcher)
    .build();
```

The composed server derives the acknowledgment and the `listChanged` flags from the provider:

- **Acknowledgment.** The server acknowledges each requested list-changed flag that
  `support()` carries, and each requested URI that `accepts_resource` accepts. `rmcp` then
  removes each flag and each resource subscription that the server capabilities do not
  advertise.
- **`listChanged`.** With a subscriptions provider set, `listChanged` on the tools, prompts
  and resources capabilities comes from `support()`. A provider with a static list supports
  the flag and never sends the notification.
- **`resources.subscribe`.** The flag stays with the info provider. Advertise it there, or
  `rmcp` drops every requested resource subscription.
- **Part sink.** `SubscriptionPart::sink` refuses a notification outside the part's accepted
  filter with `SubscriptionSendError::NotificationNotAccepted`. Once the part is cancelled,
  the sink refuses every notification with `SubscriptionSendError::SubscriptionClosed`, also a
  send still pending when the cancellation arrives.

Without a subscriptions provider, `subscriptions/listen` is method-not-found. No blanket
implementation of `SubscriptionsProvider` exists for `ServerHandler`: `ServerHandler::listen`
takes a `SubscriptionContext`, which only `rmcp` builds.

`subscriptions/listen` exists on protocol 2026-07-28 only. The composed server implements the
protocol versions that `ServerInfoProvider::supported_protocol_versions` returns; the default
is every version `rmcp` knows.

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
