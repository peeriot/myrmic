//! The bridge's one connection to its Modbus server.
//!
//! A `tokio_modbus` client context needs `&mut` for every request, and many devices
//! accept only a handful of TCP connections. So a single task owns the connection,
//! and everything else in the bridge (poll timers, cell commands) hands its
//! requests to that task through a channel. That also serialises all requests,
//! which is what a Modbus server expects anyway.
//!
//! The task connects lazily, on the first request, and again on the first request
//! after the connection broke.

use std::time::Duration;

use sorg_common::{ModbusRegister, ModbusServerAddress, ModbusWritableRegister};
use tokio::sync::{mpsc, oneshot};
use tokio_modbus::client::{Context, Reader as _, Writer as _, tcp};
use tokio_modbus::slave::SlaveContext as _;
use tokio_modbus::{ExceptionCode, Slave};

use super::codec::Cells;
use crate::wasm::cell::state::DropHandle;

/// How many requests may wait for the connection task before senders block.
const QUEUE_CAPACITY: usize = 32;

/// Reads `quantity` bits or words, starting at `address`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Read {
    pub unit: u8,
    pub register: ModbusRegister,
    pub address: u16,
    pub quantity: u16,
}

/// Writes `cells` starting at `address`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Write {
    pub unit: u8,
    pub register: ModbusWritableRegister,
    pub address: u16,
    pub cells: Cells,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub(crate) enum Error {
    /// The server answered, but with a Modbus exception.
    #[error("modbus server answered with exception: {0}")]
    Exception(ExceptionCode),
    /// No answer: the connection failed or broke.
    #[error("modbus request failed: {0}")]
    Failed(String),
    /// The server answered, but not with what the spec entry expects (e.g. too few
    /// registers). The connection is fine.
    #[error("unexpected answer from the modbus server: {0}")]
    Unexpected(String),
}

enum Job {
    Read(Read, oneshot::Sender<Result<Cells, Error>>),
    Write(Write, oneshot::Sender<Result<(), Error>>),
}

/// A handle to the connection task. Cheap to clone; the task stops once the
/// returned [`DropHandle`] is dropped.
#[derive(Clone)]
pub(crate) struct Connection {
    jobs: mpsc::Sender<Job>,
}

impl Connection {
    /// Spawns the connection task. `timeout` bounds every request, including the
    /// connect it may need first.
    pub(crate) fn spawn(server: ModbusServerAddress, timeout: Duration) -> (Self, DropHandle) {
        let (jobs, rx) = mpsc::channel(QUEUE_CAPACITY);
        let task = tokio::spawn(run(server, timeout, rx));

        (Self { jobs }, DropHandle::from(task))
    }

    pub(crate) async fn read(&self, read: Read) -> Result<Cells, Error> {
        let (reply, response) = oneshot::channel();
        self.submit(Job::Read(read, reply)).await?;
        response.await.map_err(|_| stopped())?
    }

    pub(crate) async fn write(&self, write: Write) -> Result<(), Error> {
        let (reply, response) = oneshot::channel();
        self.submit(Job::Write(write, reply)).await?;
        response.await.map_err(|_| stopped())?
    }

    async fn submit(&self, job: Job) -> Result<(), Error> {
        self.jobs.send(job).await.map_err(|_| stopped())
    }
}

fn stopped() -> Error {
    Error::Failed("the connection task has stopped".to_owned())
}

async fn run(server: ModbusServerAddress, timeout: Duration, mut jobs: mpsc::Receiver<Job>) {
    let mut context = None;

    while let Some(job) = jobs.recv().await {
        // A requester that gave up waiting has dropped its receiver; that is fine.
        match job {
            Job::Read(read, reply) => {
                let result = within(timeout, execute_read(&server, &mut context, read)).await;
                forget_if_broken(&mut context, &result);
                let _ = reply.send(result);
            }
            Job::Write(write, reply) => {
                let result = within(timeout, execute_write(&server, &mut context, write)).await;
                forget_if_broken(&mut context, &result);
                let _ = reply.send(result);
            }
        }
    }
}

/// Fails `request` if it does not finish within `timeout`.
///
/// A timed-out request counts as unanswered, so [`forget_if_broken`] drops its
/// connection. That matters beyond reconnecting: a late answer would otherwise
/// still be in the stream and be taken for the answer to the next request.
async fn within<T>(
    timeout: Duration,
    request: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout(timeout, request)
        .await
        .unwrap_or_else(|_| Err(Error::Failed(format!("no answer within {timeout:?}"))))
}

/// Drops the connection after a request got no answer, so the next request opens a
/// new one. After a Modbus exception the server did answer, so the connection is
/// kept.
fn forget_if_broken<T>(context: &mut Option<Context>, result: &Result<T, Error>) {
    if let Err(Error::Failed(err)) = result
        && context.take().is_some()
    {
        tracing::warn!("dropping the modbus connection: {err}");
    }
}

async fn execute_read(
    server: &ModbusServerAddress,
    context: &mut Option<Context>,
    read: Read,
) -> Result<Cells, Error> {
    let Read {
        unit,
        register,
        address,
        quantity,
    } = read;

    let context = connected(server, context).await?;
    context.set_slave(Slave(unit));

    let result = match register {
        ModbusRegister::Coil => context
            .read_coils(address, quantity)
            .await
            .map(|r| r.map(Cells::Bits)),
        ModbusRegister::DiscreteInput => context
            .read_discrete_inputs(address, quantity)
            .await
            .map(|r| r.map(Cells::Bits)),
        ModbusRegister::Input => context
            .read_input_registers(address, quantity)
            .await
            .map(|r| r.map(Cells::Words)),
        ModbusRegister::Holding => context
            .read_holding_registers(address, quantity)
            .await
            .map(|r| r.map(Cells::Words)),
    };

    flatten(result)
}

async fn execute_write(
    server: &ModbusServerAddress,
    context: &mut Option<Context>,
    write: Write,
) -> Result<(), Error> {
    let Write {
        unit,
        register,
        address,
        cells,
    } = write;

    let context = connected(server, context).await?;
    context.set_slave(Slave(unit));

    // A single entry uses the "write single" function codes (05, 06), which every
    // device supports; more entries need "write multiple" (15, 16).
    let result = match (register, &cells) {
        (ModbusWritableRegister::Coil, Cells::Bits(bits)) => match bits.as_slice() {
            [bit] => context.write_single_coil(address, *bit).await,
            bits => context.write_multiple_coils(address, bits).await,
        },
        (ModbusWritableRegister::Holding, Cells::Words(words)) => match words.as_slice() {
            [word] => context.write_single_register(address, *word).await,
            words => context.write_multiple_registers(address, words).await,
        },
        (register, cells) => {
            return Err(Error::Failed(format!(
                "cannot write {cells:?} to {register:?}"
            )));
        }
    };

    flatten(result)
}

/// The open connection, opened first if there is none.
async fn connected<'a>(
    server: &ModbusServerAddress,
    context: &'a mut Option<Context>,
) -> Result<&'a mut Context, Error> {
    if let Some(context) = context {
        return Ok(context);
    }

    // Resolved on every connect: the server's name may point elsewhere by now.
    let addr = tokio::net::lookup_host((server.host.as_str(), server.port))
        .await
        .map_err(|err| Error::Failed(format!("unable to resolve {}: {err}", server.host)))?
        .next()
        .ok_or_else(|| Error::Failed(format!("{} has no address", server.host)))?;

    let connection = tcp::connect(addr)
        .await
        .map_err(|err| Error::Failed(format!("unable to connect to {addr}: {err}")))?;

    Ok(context.insert(connection))
}

fn flatten<T>(result: tokio_modbus::Result<T>) -> Result<T, Error> {
    match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(exception)) => Err(Error::Exception(exception)),
        Err(err) => Err(Error::Failed(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sorg_tests::{ModbusMockHandle, SilentModbusServer};

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn read(register: ModbusRegister, address: u16, quantity: u16) -> Read {
        Read {
            unit: 1,
            register,
            address,
            quantity,
        }
    }

    fn write(register: ModbusWritableRegister, address: u16, cells: Cells) -> Write {
        Write {
            unit: 1,
            register,
            address,
            cells,
        }
    }

    #[tokio::test]
    async fn reads_all_four_tables() {
        let server = ModbusMockHandle::start().await;
        server.set_coil(1, true);
        server.set_discrete_input(2, false);
        server.set_input_registers(3, &[30]);
        server.set_holding_registers(4, &[40, 41]);

        let (connection, _task) = Connection::spawn(server.address(), TIMEOUT);

        assert_eq!(
            connection.read(read(ModbusRegister::Coil, 1, 1)).await,
            Ok(Cells::Bits(vec![true]))
        );
        assert_eq!(
            connection
                .read(read(ModbusRegister::DiscreteInput, 2, 1))
                .await,
            Ok(Cells::Bits(vec![false]))
        );
        assert_eq!(
            connection.read(read(ModbusRegister::Input, 3, 1)).await,
            Ok(Cells::Words(vec![30]))
        );
        assert_eq!(
            connection.read(read(ModbusRegister::Holding, 4, 2)).await,
            Ok(Cells::Words(vec![40, 41]))
        );
    }

    #[tokio::test]
    async fn writes_coils_and_holding_registers() {
        let server = ModbusMockHandle::start().await;
        let (connection, _task) = Connection::spawn(server.address(), TIMEOUT);

        let coil = write(ModbusWritableRegister::Coil, 5, Cells::Bits(vec![true]));
        let single = write(ModbusWritableRegister::Holding, 6, Cells::Words(vec![60]));
        let multiple = write(
            ModbusWritableRegister::Holding,
            7,
            Cells::Words(vec![70, 71]),
        );

        assert_eq!(connection.write(coil).await, Ok(()));
        assert_eq!(connection.write(single).await, Ok(()));
        assert_eq!(connection.write(multiple).await, Ok(()));

        assert_eq!(server.coil(5), Some(true));
        assert_eq!(server.holding_registers(6, 1), Some(vec![60]));
        assert_eq!(server.holding_registers(7, 2), Some(vec![70, 71]));
    }

    #[tokio::test]
    async fn a_modbus_exception_is_not_a_connection_failure() {
        let server = ModbusMockHandle::start().await;
        let (connection, _task) = Connection::spawn(server.address(), TIMEOUT);

        // Nothing was stored at 99, so the server answers IllegalDataAddress.
        assert_eq!(
            connection.read(read(ModbusRegister::Holding, 99, 1)).await,
            Err(Error::Exception(ExceptionCode::IllegalDataAddress))
        );
    }

    #[tokio::test]
    async fn reconnects_after_the_connection_broke() {
        let server = ModbusMockHandle::start().await;
        server.set_holding_registers(0, &[42]);

        let (connection, _task) = Connection::spawn(server.address(), TIMEOUT);
        let request = read(ModbusRegister::Holding, 0, 1);
        assert_eq!(
            connection.read(request.clone()).await,
            Ok(Cells::Words(vec![42]))
        );

        let server = server.restart().await;

        // The request that runs into the broken connection may fail, the one after
        // it has to succeed on a new connection.
        let _ = connection.read(request.clone()).await;
        assert_eq!(connection.read(request).await, Ok(Cells::Words(vec![42])));

        drop(server);
    }

    #[tokio::test]
    async fn a_server_that_never_answers_times_out() {
        let server = SilentModbusServer::start().await;
        let timeout = Duration::from_millis(100);
        let (connection, _task) = Connection::spawn(server.address(), timeout);

        let started = std::time::Instant::now();
        let result = connection.read(read(ModbusRegister::Holding, 0, 1)).await;

        assert!(
            matches!(&result, Err(Error::Failed(err)) if err.contains("no answer")),
            "{result:?}"
        );
        assert!(started.elapsed() < TIMEOUT, "took {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn an_unreachable_server_fails_the_request() {
        // Bind and immediately release a port, so nothing listens on it.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let server = ModbusServerAddress {
            host: addr.ip().to_string(),
            port: addr.port(),
        };

        let (connection, _task) = Connection::spawn(server, TIMEOUT);

        let result = connection.read(read(ModbusRegister::Holding, 0, 1)).await;
        assert!(matches!(result, Err(Error::Failed(_))), "{result:?}");
    }
}
