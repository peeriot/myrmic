/// A [`super::PublishNodeMetrics`] implementation that silently discards every metric.
///
/// Used as the default publisher when no real sink is configured.
pub(crate) struct NoopPublisher;

impl super::PublishNodeMetrics for NoopPublisher {
    fn publish_metric(&self, _metric: super::NodeMetric<'_>) {}
}
