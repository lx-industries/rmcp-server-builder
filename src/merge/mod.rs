//! Merging newtypes that compose several providers of one capability into one.

mod completion;
mod cursor;
mod prompts;
mod resources;
mod tools;

#[expect(
    unused_imports,
    reason = "src/lib.rs re-exports MergedCompletionProvider in a later task (task 9)"
)]
pub use completion::MergedCompletionProvider;
#[expect(
    unused_imports,
    reason = "src/lib.rs re-exports MergedPromptsProvider in a later task (task 9)"
)]
pub use prompts::MergedPromptsProvider;
#[expect(
    unused_imports,
    reason = "src/lib.rs re-exports MergedResourcesProvider in a later task (task 9)"
)]
pub use resources::MergedResourcesProvider;
#[expect(
    unused_imports,
    reason = "src/lib.rs re-exports MergedToolsProvider in a later task (task 9)"
)]
pub use tools::MergedToolsProvider;
