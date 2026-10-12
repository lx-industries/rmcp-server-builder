//! The subscriptions capability: one `subscriptions/listen` stream (protocol 2026-07-28)
//! served by a [`SubscriptionsProvider`].
//!
//! `rmcp`'s `ServerHandler::listen` takes a `SubscriptionContext`, which only `rmcp` can
//! build, so a provider cannot be one part of a stream through that interface. This
//! module defines the provider interface the composed [`Server`](crate::Server) and
//! [`MergedSubscriptionsProvider`](crate::MergedSubscriptionsProvider) use instead: a
//! [`SubscriptionPart`] carries the part's own accepted filter, a sink that enforces
//! that filter, and the listen request's context.

use std::sync::Arc;

use rmcp::{
    model::{
        ErrorData, PromptListChangedNotification, ResourceListChangedNotification,
        ResourceUpdatedNotification, ResourceUpdatedNotificationParam, ServerNotification,
        SubscriptionFilter, ToolListChangedNotification,
    },
    service::{
        RequestContext, RoleServer, SubscriptionContext, SubscriptionSendError, SubscriptionSink,
    },
};

/// The list-changed notifications a [`SubscriptionsProvider`] supports.
///
/// The composed [`Server`](crate::Server) advertises `listChanged` on the tools,
/// prompts and resources capabilities from these flags, and acknowledges a requested
/// list-changed flag only when the provider supports it. A provider with a static list
/// supports the flag and never sends the notification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListChangedSupport {
    /// `notifications/tools/list_changed`.
    pub tools: bool,
    /// `notifications/prompts/list_changed`.
    pub prompts: bool,
    /// `notifications/resources/list_changed`.
    pub resources: bool,
}

impl ListChangedSupport {
    /// Returns the flags that both `self` and `other` carry.
    #[must_use]
    pub fn intersection(self, other: Self) -> Self {
        Self {
            tools: self.tools && other.tools,
            prompts: self.prompts && other.prompts,
            resources: self.resources && other.resources,
        }
    }
}

/// Provider for the subscriptions capability: `subscriptions/listen`.
///
/// `rmcp` acknowledges a listen request before it runs the stream, and the
/// acknowledgment is synchronous: [`support`](Self::support) and
/// [`accepts_resource`](Self::accepts_resource) answer from state the provider holds,
/// with no request to make.
pub trait SubscriptionsProvider: Send + Sync + 'static {
    /// The list-changed notifications this provider supports.
    fn support(&self) -> ListChangedSupport;

    /// Whether this provider sends `notifications/resources/updated` for `uri`.
    fn accepts_resource(&self, uri: &str) -> bool;

    /// Runs this provider's part of one stream until the part ends.
    ///
    /// `Ok(())` ends the stream gracefully, an error ends it with that error. Return
    /// when [`SubscriptionPart::cancelled`] completes.
    fn listen(&self, part: SubscriptionPart) -> impl Future<Output = Result<(), ErrorData>> + Send;
}

/// Computes the filter a provider acknowledges for `requested`: each requested
/// list-changed flag that `support` carries, and each requested URI that
/// `accepts_resource` accepts. No accepted URI leaves `resource_subscriptions` out.
pub(crate) fn accepted_filter(
    support: ListChangedSupport,
    accepts_resource: impl Fn(&str) -> bool,
    requested: &SubscriptionFilter,
) -> SubscriptionFilter {
    let mut accepted = SubscriptionFilter::new();
    accepted.tools_list_changed =
        (requested.tools_list_changed == Some(true) && support.tools).then_some(true);
    accepted.prompts_list_changed =
        (requested.prompts_list_changed == Some(true) && support.prompts).then_some(true);
    accepted.resources_list_changed =
        (requested.resources_list_changed == Some(true) && support.resources).then_some(true);
    accepted.resource_subscriptions = requested
        .resource_subscriptions
        .as_ref()
        .map(|uris| {
            uris.iter()
                .filter(|uri| accepts_resource(uri))
                .cloned()
                .collect::<Vec<_>>()
        })
        .filter(|uris| !uris.is_empty());
    accepted
}

/// One provider's part of a `subscriptions/listen` stream.
#[derive(Debug)]
pub struct SubscriptionPart {
    sink: SubscriptionPartSink,
    context: RequestContext<RoleServer>,
}

impl SubscriptionPart {
    /// A part that covers the whole stream: its acknowledged filter and its request context.
    pub(crate) fn from_context(context: &SubscriptionContext) -> Self {
        Self {
            sink: SubscriptionPartSink {
                stream_sink: context.sink().clone(),
                accepted: Arc::new(context.accepted().clone()),
            },
            context: context.request_context().clone(),
        }
    }

    /// A part of this part, restricted to `accepted`, with its own cancellation token: a
    /// child of this part's token.
    pub(crate) fn narrowed(&self, accepted: SubscriptionFilter) -> Self {
        let mut context = self.context.clone();
        context.ct = self.context.ct.child_token();
        Self {
            sink: SubscriptionPartSink {
                stream_sink: self.sink.stream_sink.clone(),
                accepted: Arc::new(accepted),
            },
            context,
        }
    }

    /// The notifications this part may send.
    pub fn accepted(&self) -> &SubscriptionFilter {
        self.sink.accepted()
    }

    /// The sink this part sends through; it refuses a notification outside
    /// [`accepted`](Self::accepted).
    pub fn sink(&self) -> &SubscriptionPartSink {
        &self.sink
    }

    /// The listen request's context: its `_meta`, the peer, and this part's
    /// cancellation token. Send a custom notification through the peer.
    pub fn request_context(&self) -> &RequestContext<RoleServer> {
        &self.context
    }

    /// Completes when this part is cancelled: by the client, or because another part
    /// ended the stream.
    pub async fn cancelled(&self) {
        self.context.ct.cancelled().await;
    }
}

/// A notification sink scoped to one [`SubscriptionPart`].
///
/// It refuses a notification outside its part's accepted filter, then sends through the
/// stream's `rmcp` sink, which adds the subscription id. The `rmcp` client cancels a
/// stream on a notification outside the acknowledged filter; the part check also stops
/// a part from sending a notification another part owns.
#[derive(Clone, Debug)]
pub struct SubscriptionPartSink {
    stream_sink: SubscriptionSink,
    accepted: Arc<SubscriptionFilter>,
}

impl SubscriptionPartSink {
    /// The notifications this sink sends.
    pub fn accepted(&self) -> &SubscriptionFilter {
        self.accepted.as_ref()
    }

    /// Sends `notification` when this part's filter accepts it.
    ///
    /// # Errors
    ///
    /// [`SubscriptionSendError::NotificationNotAccepted`] for a list-changed or
    /// resource-updated notification outside this part's filter; otherwise every error
    /// of `rmcp`'s `SubscriptionSink::send`.
    pub async fn send(
        &self,
        notification: ServerNotification,
    ) -> Result<(), SubscriptionSendError> {
        if let Some(method) = refused_method(&self.accepted, &notification) {
            return Err(SubscriptionSendError::NotificationNotAccepted(method));
        }
        self.stream_sink.send(notification).await
    }

    /// Sends `notifications/tools/list_changed`. See [`Self::send`].
    pub async fn notify_tool_list_changed(&self) -> Result<(), SubscriptionSendError> {
        self.send(ServerNotification::ToolListChangedNotification(
            ToolListChangedNotification::default(),
        ))
        .await
    }

    /// Sends `notifications/prompts/list_changed`. See [`Self::send`].
    pub async fn notify_prompt_list_changed(&self) -> Result<(), SubscriptionSendError> {
        self.send(ServerNotification::PromptListChangedNotification(
            PromptListChangedNotification::default(),
        ))
        .await
    }

    /// Sends `notifications/resources/list_changed`. See [`Self::send`].
    pub async fn notify_resource_list_changed(&self) -> Result<(), SubscriptionSendError> {
        self.send(ServerNotification::ResourceListChangedNotification(
            ResourceListChangedNotification::default(),
        ))
        .await
    }

    /// Sends `notifications/resources/updated` for `uri`. See [`Self::send`].
    pub async fn notify_resource_updated(
        &self,
        uri: impl Into<String>,
    ) -> Result<(), SubscriptionSendError> {
        self.send(ServerNotification::ResourceUpdatedNotification(
            ResourceUpdatedNotification::new(ResourceUpdatedNotificationParam::new(uri)),
        ))
        .await
    }
}

/// The method of `notification` when `accepted` refuses it; `None` when the part filter
/// has no rule for it and the stream sink decides.
fn refused_method(
    accepted: &SubscriptionFilter,
    notification: &ServerNotification,
) -> Option<&'static str> {
    match notification {
        ServerNotification::ToolListChangedNotification(_) => (accepted.tools_list_changed
            != Some(true))
        .then_some("notifications/tools/list_changed"),
        ServerNotification::PromptListChangedNotification(_) => (accepted.prompts_list_changed
            != Some(true))
        .then_some("notifications/prompts/list_changed"),
        ServerNotification::ResourceListChangedNotification(_) => (accepted.resources_list_changed
            != Some(true))
        .then_some("notifications/resources/list_changed"),
        ServerNotification::ResourceUpdatedNotification(update) => (!accepted
            .resource_subscriptions
            .as_ref()
            .is_some_and(|uris| uris.contains(&update.params.uri)))
        .then_some("notifications/resources/updated"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rmcp::model::SubscriptionFilter;

    use super::{ListChangedSupport, accepted_filter};

    #[test]
    fn the_acknowledgment_keeps_requested_flags_the_provider_supports() {
        let requested = SubscriptionFilter::builder()
            .tools_list_changed()
            .prompts_list_changed()
            .build();
        let support = ListChangedSupport {
            tools: true,
            prompts: false,
            resources: true,
        };

        let accepted = accepted_filter(support, |_| false, &requested);

        assert_eq!(accepted.tools_list_changed, Some(true));
        assert_eq!(accepted.prompts_list_changed, None);
        assert_eq!(accepted.resources_list_changed, None, "not requested");
    }

    #[test]
    fn the_acknowledgment_keeps_only_accepted_uris() {
        let requested = SubscriptionFilter::builder()
            .resource_subscriptions(["a://1", "b://1"])
            .build();

        let accepted = accepted_filter(
            ListChangedSupport::default(),
            |uri| uri.starts_with("a://"),
            &requested,
        );

        assert_eq!(
            accepted.resource_subscriptions,
            Some(vec!["a://1".to_owned()])
        );
    }

    #[test]
    fn no_accepted_uri_leaves_the_resource_subscriptions_out() {
        let requested = SubscriptionFilter::builder()
            .resource_subscriptions(["b://1"])
            .build();

        let accepted = accepted_filter(ListChangedSupport::default(), |_| false, &requested);

        assert_eq!(accepted.resource_subscriptions, None);
    }

    #[test]
    fn the_intersection_keeps_the_flags_both_carry() {
        let left = ListChangedSupport {
            tools: true,
            prompts: true,
            resources: false,
        };
        let right = ListChangedSupport {
            tools: true,
            prompts: false,
            resources: true,
        };

        assert_eq!(
            left.intersection(right),
            ListChangedSupport {
                tools: true,
                prompts: false,
                resources: false
            }
        );
    }
}
