//! Zenoh utilities for sending and receiving data.

use core::pin::pin;

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};

use zenoh_traits::{Close, Error, ErrorKind, Receiver, SendPayload, Sender, Session};

use crate::io::{StreamConsumer, StreamProducer};

/// Send a message to the provided sender.
///
/// # Arguments
/// - `sender`: The sender to use for sending the message.
/// - `encoding`: The encoding to use for the sent message.
/// - `producer`: The producer that will produce the message to be sent.
/// - `progress_notify`: A function to notify progress.
///
/// # Returns
/// - `Ok(())`: The sending completed successfully.
/// - `Err(P::Error)`: An error occurred during sending or producing.
pub async fn send<S, P, N>(
    mut sender: S,
    encoding: &str,
    mut producer: P,
    progress_notify: N,
) -> Result<(), P::Error>
where
    S: Sender,
    P: StreamProducer,
    P::Error: From<ErrorKind>,
    N: FnMut(),
{
    let payload = sender.send().await.map_err(|e| e.kind())?;
    let mut write = payload
        .with_encoding(encoding)
        .await
        .map_err(|e| e.kind())?;

    producer.produce(&mut write, progress_notify).await?;

    write.close().await.map_err(|e| e.kind())?;

    Ok(())
}

/// Receive a message from the provided receiver.
///
/// # Arguments
/// - `receiver`: The receiver to use for receiving the message.
/// - `read_wrapper`: A factory to use for wrapping the read stream returned by the receiver.
/// - `consumer`: The consumer that will consume the received message.
/// - `progress_notify`: A function to notify progress.
///
/// # Returns
/// - `Ok(true)`: A message was received and consumed successfully.
/// - `Ok(false)`: No message was received.
/// - `Err(D::Error)`: An error occurred during receiving or consuming.
pub async fn receive<R, C, N>(
    mut receiver: R,
    mut consumer: C,
    progress_notify: N,
) -> Result<bool, C::Error>
where
    R: Receiver,
    C: StreamConsumer,
    C::Error: From<ErrorKind>,
    N: FnMut(),
{
    let Ok((_, mut read)) = receiver.receive().await else {
        return Ok(false);
    };

    consumer.consume(&mut read, progress_notify).await?;

    read.close().await.map_err(|e| e.kind())?;

    Ok(true)
}

/// Set a message on the provided topic.
///
/// # Arguments
/// - `session`: The Zenoh session to use for communication.
/// - `topic`: The topic to set the message on.
/// - `producer`: The producer that will produce the message to be set.
/// - `progress_notify`: A function to notify progress.
pub async fn set<S, P, N>(
    session: S,
    topic: &str,
    mut producer: P,
    mut progress_notify: N,
) -> Result<(), P::Error>
where
    S: Session,
    P: StreamProducer,
    P::Error: From<ErrorKind>,
    N: FnMut(),
{
    info!("About to produce data on topic {}...", topic);

    let mut setter = session.set(topic).await.map_err(|e| e.kind())?;

    loop {
        send(&mut setter, "", &mut producer, &mut progress_notify).await?;

        info!("Produced data on topic {}", topic);
    }

    #[allow(unreachable_code)]
    Ok(())
}

/// Get a message from the provided topic.
///
/// # Arguments
/// - `session`: The Zenoh session to use for communication.
/// - `topic`: The topic to get the message from.
/// - `retry_timeout`: The duration to wait before retrying to get the message.
/// - `consumer`: The consumer that will consume the received message.
/// - `progress_notify`: A function to notify progress.
pub async fn get<S, C, N>(
    session: S,
    topic: &str,
    retry_timeout: Duration,
    mut consumer: C,
    mut progress_notify: N,
) -> Result<(), C::Error>
where
    S: Session,
    C: StreamConsumer,
    C::Error: From<ErrorKind>,
    N: FnMut(),
{
    loop {
        let getter = session.get(topic).await.map_err(|e| e.kind())?;

        info!("Waiting for data on topic: {}...", topic);

        let mut receive = pin!(receive(getter, &mut consumer, &mut progress_notify));
        let mut timeout = pin!(Timer::after(retry_timeout));

        if let Either::First(res) = select(&mut receive, &mut timeout).await {
            if res? {
                info!("Successfully consumed data on topic: {}", topic);
                break Ok(());
            } else {
                Timer::after(retry_timeout).await;
            }
        }
    }
}
