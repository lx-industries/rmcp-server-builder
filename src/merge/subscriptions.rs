//! Merges several [`SubscriptionsProvider`]s into one subscriptions capability.
//!
//! One `subscriptions/listen` stream becomes one part per composed provider. Each part
//! carries the stream's accepted list-changed flags and the accepted URIs its provider
//! owns. The first part to end ends the stream: an error propagates, a graceful end
//! stays graceful, and every other part is cancelled.

use std::pin::Pin;
use std::task::Poll;

use rmcp::model::ErrorData;

use crate::subscriptions::{
    ListChangedSupport, SubscriptionPart, SubscriptionsProvider, accepted_filter,
};

/// Object-safe adapter over [`SubscriptionsProvider`], boxing its future, so
/// [`MergedSubscriptionsProvider`] can store `Vec<Box<dyn DynSubscriptionsProvider>>`.
trait DynSubscriptionsProvider: Send + Sync {
    fn support(&self) -> ListChangedSupport;
    fn accepts_resource(&self, uri: &str) -> bool;
    fn listen<'provider>(
        &'provider self,
        part: SubscriptionPart,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>>;
}

impl<T: SubscriptionsProvider> DynSubscriptionsProvider for T {
    fn support(&self) -> ListChangedSupport {
        SubscriptionsProvider::support(self)
    }

    fn accepts_resource(&self, uri: &str) -> bool {
        SubscriptionsProvider::accepts_resource(self, uri)
    }

    fn listen<'provider>(
        &'provider self,
        part: SubscriptionPart,
    ) -> Pin<Box<dyn Future<Output = Result<(), ErrorData>> + Send + 'provider>> {
        Box::pin(SubscriptionsProvider::listen(self, part))
    }
}

/// Composes several [`SubscriptionsProvider`]s into one subscriptions capability.
///
/// * `support` is the intersection of every provider's support; an empty composition
///   supports nothing.
/// * `accepts_resource` accepts a URI that exactly one provider accepts: zero or two
///   owners leave it out of the acknowledgment.
/// * `listen` runs every provider's part, also a part with an empty filter: a provider
///   MAY send custom notifications through the peer. The first part to end ends the
///   stream with its outcome; the other parts are cancelled and awaited first.
pub struct MergedSubscriptionsProvider {
    providers: Vec<Box<dyn DynSubscriptionsProvider>>,
}

impl MergedSubscriptionsProvider {
    /// Starts an empty composition.
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    /// Adds `provider` to the composition, after every provider added so far.
    #[must_use]
    pub fn with_provider<T: SubscriptionsProvider>(mut self, provider: T) -> Self {
        self.providers.push(Box::new(provider));
        self
    }
}

impl Default for MergedSubscriptionsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionsProvider for MergedSubscriptionsProvider {
    fn support(&self) -> ListChangedSupport {
        let mut supports = self.providers.iter().map(|provider| provider.support());
        let Some(first) = supports.next() else {
            return ListChangedSupport::default();
        };
        supports.fold(first, ListChangedSupport::intersection)
    }

    fn accepts_resource(&self, uri: &str) -> bool {
        self.providers
            .iter()
            .filter(|provider| provider.accepts_resource(uri))
            .count()
            == 1
    }

    async fn listen(&self, part: SubscriptionPart) -> Result<(), ErrorData> {
        if self.providers.is_empty() {
            part.cancelled().await;
            return Ok(());
        }
        let parts: Vec<SubscriptionPart> = self
            .providers
            .iter()
            .map(|provider| {
                part.narrowed(accepted_filter(
                    provider.support(),
                    |uri| provider.accepts_resource(uri),
                    part.accepted(),
                ))
            })
            .collect();
        let part_cancellations: Vec<_> = parts
            .iter()
            .map(|part| part.request_context().ct.clone())
            .collect();
        let mut listens: Vec<_> = self
            .providers
            .iter()
            .zip(parts)
            .map(|(provider, part)| provider.listen(part))
            .collect();

        let (ended_index, outcome) = first_to_end(&mut listens).await;

        for cancellation in &part_cancellations {
            cancellation.cancel();
        }
        for (index, listen) in listens.into_iter().enumerate() {
            if index != ended_index {
                // The first part's end decides the stream's outcome; a cancelled
                // part's own outcome is not reported.
                let _ = listen.await;
            }
        }
        outcome
    }
}

/// Polls every future until one completes; returns its index and output. The other
/// futures stay unfinished in `futures`.
async fn first_to_end<F: Future + Unpin>(futures: &mut [F]) -> (usize, F::Output) {
    std::future::poll_fn(|waker_context| {
        for (index, future) in futures.iter_mut().enumerate() {
            if let Poll::Ready(output) = Pin::new(future).poll(waker_context) {
                return Poll::Ready((index, output));
            }
        }
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use rmcp::model::ErrorData;

    use super::MergedSubscriptionsProvider;
    use crate::subscriptions::{ListChangedSupport, SubscriptionPart, SubscriptionsProvider};

    /// Owns every URI with its prefix and supports a fixed set of list-changed flags.
    struct Fixed {
        prefix: &'static str,
        support: ListChangedSupport,
    }

    impl Fixed {
        fn owning(prefix: &'static str) -> Self {
            Self {
                prefix,
                support: ListChangedSupport::default(),
            }
        }
    }

    impl SubscriptionsProvider for Fixed {
        fn support(&self) -> ListChangedSupport {
            self.support
        }

        fn accepts_resource(&self, uri: &str) -> bool {
            uri.starts_with(self.prefix)
        }

        async fn listen(&self, part: SubscriptionPart) -> Result<(), ErrorData> {
            part.cancelled().await;
            Ok(())
        }
    }

    #[test]
    fn support_is_the_intersection_of_every_provider() {
        let merged = MergedSubscriptionsProvider::new()
            .with_provider(Fixed {
                prefix: "a://",
                support: ListChangedSupport {
                    tools: true,
                    prompts: true,
                    resources: false,
                },
            })
            .with_provider(Fixed {
                prefix: "b://",
                support: ListChangedSupport {
                    tools: true,
                    prompts: false,
                    resources: false,
                },
            });

        assert_eq!(
            merged.support(),
            ListChangedSupport {
                tools: true,
                prompts: false,
                resources: false
            }
        );
    }

    #[test]
    fn an_empty_composition_supports_nothing_and_accepts_nothing() {
        let merged = MergedSubscriptionsProvider::new();

        assert_eq!(merged.support(), ListChangedSupport::default());
        assert!(!merged.accepts_resource("a://1"));
    }

    #[test]
    fn a_uri_with_exactly_one_owner_is_accepted() {
        let merged = MergedSubscriptionsProvider::new()
            .with_provider(Fixed::owning("a://"))
            .with_provider(Fixed::owning("b://"));

        assert!(merged.accepts_resource("a://1"));
    }

    #[test]
    fn a_uri_with_two_owners_or_none_is_refused() {
        let merged = MergedSubscriptionsProvider::new()
            .with_provider(Fixed::owning("a://"))
            .with_provider(Fixed::owning("a://"));

        assert!(!merged.accepts_resource("a://1"), "two owners");
        assert!(!merged.accepts_resource("c://1"), "no owner");
    }
}
