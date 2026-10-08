//! Merging newtypes that compose several providers of one capability into one.

mod cursor;
mod prompts;
mod tools;

#[expect(
    unused_imports,
    reason = "src/lib.rs re-exports MergedPromptsProvider in a later task (task 9)"
)]
pub use prompts::MergedPromptsProvider;
#[expect(
    unused_imports,
    reason = "src/lib.rs re-exports MergedToolsProvider in a later task (task 9)"
)]
pub use tools::MergedToolsProvider;
