pub type PoisonRcv = tokio::sync::oneshot::Receiver<()>;
pub type PoisonSnd = tokio::sync::oneshot::Sender<()>;
#[must_use]
pub fn poison_channel() -> (PoisonSnd, PoisonRcv) {
    tokio::sync::oneshot::channel()
}
