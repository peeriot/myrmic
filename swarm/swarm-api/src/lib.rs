pub type DropSender = flume::Sender<()>;
pub type DropNotifier = flume::Receiver<()>;
pub type Ready = std::sync::Arc<tokio::sync::Notify>;
