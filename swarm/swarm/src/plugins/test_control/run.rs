use swarm_api::DropNotifier;
use tokio::sync::oneshot;
use tracing::{debug, error};
use zenoh::Session;

pub(super) async fn run(session: Session, drop_rx: DropNotifier) {
    let (poison_snd, poison_rcv) = oneshot::channel();
    tokio::spawn(async move {
        let _ = drop_rx.recv_async().await;
        let _ = poison_snd.send(());
    });

    debug!("spawning test control");
    match test_control::spawn(session, poison_rcv).await {
        Ok(()) => debug!("test control terminated"),
        Err(err) => error!("test control terminated with an error: {err}"),
    }
}
