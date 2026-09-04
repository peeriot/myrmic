use test_control_common::{PoisonRcv, bail, poison_channel, set_up_queryable};
use tracing::{debug, info};
use zenoh::Session;

use crate::{Result, event_loop::set_up_event_loop, queryables::Queryable};

pub async fn spawn(session: Session, off_rcv: PoisonRcv) -> Result<()> {
    info!("spawning test control");

    let (_poison_snd_event_loop, poison_rcv_event_loop) = poison_channel();
    let (client, handle_event_loop) = set_up_event_loop(session.clone(), poison_rcv_event_loop);

    let (handle_create_publisher, _poison_snd_create_publisher) =
        set_up_queryable(session.clone(), client.handle(), Queryable::CreatePublisher);
    let (handle_delete_publisher, _poison_snd_delete_publisher) =
        set_up_queryable(session.clone(), client.handle(), Queryable::DeletePublisher);

    let (handle_create_subscriber, _poison_snd_create_subscriber) = set_up_queryable(
        session.clone(),
        client.handle(),
        Queryable::CreateSubscriber,
    );
    let (handle_delete_subscriber, _poison_snd_delete_subscriber) = set_up_queryable(
        session.clone(),
        client.handle(),
        Queryable::DeleteSubscriber,
    );

    let (handle_create_queryable, _poison_snd_create_queryable) =
        set_up_queryable(session.clone(), client.handle(), Queryable::CreateQueryable);
    let (handle_delete_queryable, _poison_snd_delete_queryable) =
        set_up_queryable(session.clone(), client.handle(), Queryable::DeleteQueryable);

    let (handle_put, _poison_snd_put) =
        set_up_queryable(session.clone(), client.handle(), Queryable::Put);
    let (handle_get, _poison_snd_get) =
        set_up_queryable(session.clone(), client.handle(), Queryable::Get);
    let (handle_delete, _poison_snd_delete) =
        set_up_queryable(session.clone(), client.handle(), Queryable::Delete);
    let (handle_stats, _poison_snd_stats) =
        set_up_queryable(session.clone(), client.handle(), Queryable::Stats);
    let (handle_health, _poison_snd_health) =
        set_up_queryable(session.clone(), client.handle(), Queryable::Health);
    let (handle_introspection, _poison_snd_introspection) =
        set_up_queryable(session.clone(), client.handle(), Queryable::Introspection);

    tokio::select! {
        exec_result = handle_event_loop => {
            let exec_result = match exec_result{
                Ok(res) => res,
                Err(join_err) => {
                    bail!("the event loop has panicked or was canceled: {join_err}");
                }
            };
            match exec_result {
                Ok(()) => bail!("event loop terminated without error"),
                Err(err) => bail!("event loop terminated due to error: '{err}'")
            }
        }

        _ = handle_create_publisher => {
            bail!("create publisher terminated");
        }

        _ = handle_delete_publisher => {
            bail!("delete publisher terminated");
        }

        _ = handle_create_subscriber => {
            bail!("create subscriber terminated");
        }

        _ = handle_delete_subscriber => {
            bail!("delete subscriber terminated");
        }

        _ = handle_create_queryable => {
            bail!("create queryable terminated");
        }

        _ = handle_delete_queryable => {
            bail!("delete queryable terminated");
        }

        _ = handle_put => {
            bail!("put queryable terminated");
        }

        _ = handle_get => {
            bail!("get queryable terminated");
        }

        _ = handle_delete => {
            bail!("delete queryable terminated");
        }

        _ = handle_stats => {
            bail!("stats queryable terminated");
        }

        _ = handle_health => {
            bail!("health queryable terminated");
        }

        _ = handle_introspection => {
            bail!("introspection queryable terminated");
        }

        _ = off_rcv => {
            debug!("test control received shutdown signal");
        }
    }

    debug!("shutting down test control");

    Ok(())
}
