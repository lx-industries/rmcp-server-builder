//! Merging newtypes that compose several providers of one capability into one.

mod completion;
mod cursor;
mod logging;
mod prompts;
mod resources;
mod tools;

pub use completion::MergedCompletionProvider;
pub use logging::MergedLoggingProvider;
pub use prompts::MergedPromptsProvider;
pub use resources::MergedResourcesProvider;
pub use tools::MergedToolsProvider;
