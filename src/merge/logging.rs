//! Merges several [`LoggingProvider`]s into one logging capability.
//!
//! [`MergedLoggingProvider`] fans a `set_level` call out to every composed provider.

use std::pin::Pin;

use rmcp::{
    model::ErrorData,
    service::{RequestContext, RoleServer},
};

#[expect(
    deprecated,
    reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
)]
use rmcp::model::SetLevelRequestParams;

use crate::providers::LoggingProvider;

/// Object-safe adapter over [`LoggingProvider`], boxing its future.
///
/// `LoggingProvider::set_level` is written as `-> impl Future<Output = ...> + Send`
/// (return-position impl trait), which is not object-safe: `Box<dyn LoggingProvider>`
/// cannot be named. This sealed trait is implemented for every `T: LoggingProvider`
/// through a blanket implementation below, boxing the future so
/// [`MergedLoggingProvider`] can store `Vec<Box<dyn DynLoggingProvider>>` internally,
/// without changing `LoggingProvider`'s public, unboxed shape.
trait DynLoggingProvider: Send + Sync {
    /// Object-safe counterpart of [`LoggingProvider::set_level`].
    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    fn set_level<'provider>(
        &'provider self,
        request: SetLevelRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>>;
}

impl<T: LoggingProvider> DynLoggingProvider for T {
    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    fn set_level<'provider>(
        &'provider self,
        request: SetLevelRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>> {
        Box::pin(LoggingProvider::set_level(self, request, context))
    }
}

/// Composes several [`LoggingProvider`]s into one logging capability.
///
/// `set_level` fans the request out to every composed provider, sequentially, in
/// construction order: it awaits one provider before invoking the next.
///
/// # Fan-out on failure
///
/// `set_level` invokes every composed provider, even after an earlier one answers
/// `Err`: a provider later in the composition still receives the level change
/// attempt. When one or more providers answer `Err`, [`MergedLoggingProvider::set_level`]
/// returns the first such error; it never short-circuits the remaining providers and
/// never silently swallows a failure.
pub struct MergedLoggingProvider {
    /// The composed providers, in fan-out order.
    providers: Vec<Box<dyn DynLoggingProvider>>,
}

impl MergedLoggingProvider {
    /// Starts an empty composition.
    ///
    /// Add providers with [`MergedLoggingProvider::with_provider`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Adds `provider` to the composition, after every provider added so far.
    ///
    /// A provider's position among `with_provider` calls is its position in the
    /// sequential fan-out [`MergedLoggingProvider::set_level`] performs.
    #[must_use]
    pub fn with_provider<T: LoggingProvider>(mut self, provider: T) -> Self {
        self.providers.push(Box::new(provider));
        self
    }
}

impl Default for MergedLoggingProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LoggingProvider for MergedLoggingProvider {
    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    async fn set_level(
        &self,
        request: SetLevelRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let mut first_error = None;
        for provider in &self.providers {
            if let Err(error) = provider.set_level(request.clone(), context.clone()).await {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rmcp::ServiceExt;
    use rmcp::model::{ClientCapabilities, ClientConfig, ErrorCode, Implementation, RequestId};
    use rmcp::service::Peer;

    use super::*;

    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    use rmcp::model::LoggingLevel;

    /// The last [`SetLevelRequestParams`] a [`LoggingStub`] saw, shared with the test
    /// that moves the stub into a [`MergedLoggingProvider`] composition.
    #[derive(Default)]
    struct LoggingStubState {
        /// The last level recorded, or `None` before the first `set_level` call.
        #[expect(
            deprecated,
            reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
        )]
        last_request: Mutex<Option<SetLevelRequestParams>>,
    }

    /// A [`LoggingProvider`] stub recording the last [`SetLevelRequestParams`] it saw,
    /// and optionally always answering `Err`.
    struct LoggingStub {
        /// Shared state a test reads after the stub moves into a composition.
        state: std::sync::Arc<LoggingStubState>,
        /// Whether this stub's `set_level` always answers `Err`.
        always_errors: bool,
    }

    impl LoggingStub {
        /// Builds a stub whose `set_level` records the request and answers `Ok(())`.
        fn new() -> (Self, std::sync::Arc<LoggingStubState>) {
            let state = std::sync::Arc::new(LoggingStubState::default());
            (
                Self {
                    state: state.clone(),
                    always_errors: false,
                },
                state,
            )
        }

        /// Builds a stub whose `set_level` records the request, then answers `Err`.
        fn always_erroring() -> (Self, std::sync::Arc<LoggingStubState>) {
            let state = std::sync::Arc::new(LoggingStubState::default());
            (
                Self {
                    state: state.clone(),
                    always_errors: true,
                },
                state,
            )
        }
    }

    impl LoggingStubState {
        /// The level this state's `set_level` last recorded, if any.
        #[expect(
            deprecated,
            reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
        )]
        fn last_level(&self) -> Option<LoggingLevel> {
            self.last_request
                .lock()
                .expect("last_request mutex poisoned")
                .as_ref()
                .map(|request| request.level)
        }
    }

    impl LoggingProvider for LoggingStub {
        #[expect(
            deprecated,
            reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
        )]
        async fn set_level(
            &self,
            request: SetLevelRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<(), ErrorData> {
            *self
                .state
                .last_request
                .lock()
                .expect("last_request mutex poisoned") = Some(request);
            if self.always_errors {
                Err(ErrorData::internal_error("stub always errors", None))
            } else {
                Ok(())
            }
        }
    }

    /// A [`RequestContext<RoleServer>`] usable in a pure unit test.
    ///
    /// `rmcp`'s `Peer` has no public constructor outside a live client/server
    /// handshake, so this mints one over an in-memory duplex transport and then
    /// discards the connection: `MergedLoggingProvider` and `LoggingStub` never read
    /// from `context.peer`, they only forward it, so a torn-down peer is a valid
    /// stand-in.
    async fn test_context() -> RequestContext<RoleServer> {
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        let server = crate::ServerBuilder::new()
            .info(Implementation::new("merge-logging-test", "0.0.0"))
            .build();
        let server_task = tokio::spawn(async move {
            let running = server.serve(server_transport).await.expect("serve server");
            let peer: Peer<RoleServer> = running.peer().clone();
            running.waiting().await.expect("server stops");
            peer
        });

        let client_configuration = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("merge-logging-test-client", "0.0.0"),
        );
        let client = client_configuration
            .serve(client_transport)
            .await
            .expect("initialize client");
        client.cancel().await.expect("cancel client");
        let peer = server_task.await.expect("server task");

        RequestContext::new(RequestId::Number(0), peer)
    }

    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    #[tokio::test]
    async fn set_level_reaches_every_composed_provider() {
        let (first, first_state) = LoggingStub::new();
        let (second, second_state) = LoggingStub::new();
        let merged = MergedLoggingProvider::new()
            .with_provider(first)
            .with_provider(second);

        LoggingProvider::set_level(
            &merged,
            SetLevelRequestParams::new(LoggingLevel::Warning),
            test_context().await,
        )
        .await
        .expect("set_level succeeds");

        assert_eq!(first_state.last_level(), Some(LoggingLevel::Warning));
        assert_eq!(second_state.last_level(), Some(LoggingLevel::Warning));
    }

    #[expect(
        deprecated,
        reason = "rmcp 3.x deprecates logging (SEP-2577); legacy protocol versions still dispatch logging/setLevel"
    )]
    #[tokio::test]
    async fn set_level_still_invokes_the_other_provider_after_one_errors() {
        let (erroring, _erroring_state) = LoggingStub::always_erroring();
        let (healthy, healthy_state) = LoggingStub::new();
        let merged = MergedLoggingProvider::new()
            .with_provider(erroring)
            .with_provider(healthy);

        let error = LoggingProvider::set_level(
            &merged,
            SetLevelRequestParams::new(LoggingLevel::Error),
            test_context().await,
        )
        .await
        .expect_err("set_level reports the failure");

        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(
            healthy_state.last_level(),
            Some(LoggingLevel::Error),
            "the healthy provider is still invoked despite the erroring one"
        );
    }
}
