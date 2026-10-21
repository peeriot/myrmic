//! In-process Modbus TCP servers for tests of the Modbus bridge.
//!
//! [`ModbusMockHandle`] keeps the four data tables in memory. Reading an address
//! that was never set answers with the `IllegalDataAddress` exception, like a
//! device that does not map that address; writing always succeeds.
//! [`SilentModbusServer`] accepts connections and never answers.

use std::collections::HashMap;
use std::future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::net::TcpListener;
use tokio_modbus::server::Service;
use tokio_modbus::server::tcp::{Server, accept_tcp_connection};
use tokio_modbus::{ExceptionCode, Request, Response};

/// Aborts the server task when the handle owning it is dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Debug, Default)]
struct Tables {
    coils: HashMap<u16, bool>,
    discrete_inputs: HashMap<u16, bool>,
    input_registers: HashMap<u16, u16>,
    holding_registers: HashMap<u16, u16>,
}

/// A running test server. Dropping it stops the server and closes all of its
/// connections.
pub struct ModbusMockHandle {
    addr: SocketAddr,
    tables: Arc<Mutex<Tables>>,
    _task: AbortOnDrop,
}

impl ModbusMockHandle {
    /// Starts a server on a free port on localhost.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        Self::serve(listener, Arc::default())
    }

    /// Stops `self` and starts a new server on the same address, with the same
    /// tables: a device that went away and came back. Its clients' connections
    /// break.
    pub async fn restart(self) -> Self {
        let Self {
            addr,
            tables,
            _task: task,
        } = self;
        drop(task);

        // The aborted task releases the port once the runtime gets to drop it.
        let mut attempts = 0;
        let listener = loop {
            match TcpListener::bind(addr).await {
                Ok(listener) => break listener,
                Err(err) if attempts < 100 => {
                    attempts += 1;
                    tracing::debug!("port {addr} not released yet: {err}");
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(err) => panic!("unable to rebind {addr}: {err}"),
            }
        };

        Self::serve(listener, tables)
    }

    fn serve(listener: TcpListener, tables: Arc<Mutex<Tables>>) -> Self {
        let addr = listener.local_addr().unwrap();
        let service = MockService(tables.clone());

        let task = tokio::spawn(async move {
            let on_connected = |stream, socket_addr| {
                let service = service.clone();
                async move { accept_tcp_connection(stream, socket_addr, |_| Ok(Some(service.clone()))) }
            };
            let _ = Server::new(listener).serve(&on_connected, |_| {}).await;
        });

        Self {
            addr,
            tables,
            _task: AbortOnDrop(task),
        }
    }

    /// The address of this server, as a bridge connects to it.
    pub fn address(&self) -> sorg_common::ModbusServerAddress {
        server_address(self.addr)
    }

    fn tables(&self) -> std::sync::MutexGuard<'_, Tables> {
        self.tables.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_coil(&self, address: u16, value: bool) {
        self.tables().coils.insert(address, value);
    }

    pub fn set_discrete_input(&self, address: u16, value: bool) {
        self.tables().discrete_inputs.insert(address, value);
    }

    pub fn set_input_registers(&self, address: u16, words: &[u16]) {
        insert_words(&mut self.tables().input_registers, address, words);
    }

    pub fn set_holding_registers(&self, address: u16, words: &[u16]) {
        insert_words(&mut self.tables().holding_registers, address, words);
    }

    pub fn coil(&self, address: u16) -> Option<bool> {
        self.tables().coils.get(&address).copied()
    }

    pub fn holding_registers(&self, address: u16, quantity: u16) -> Option<Vec<u16>> {
        read(&self.tables().holding_registers, address, quantity).ok()
    }
}

#[derive(Clone)]
struct MockService(Arc<Mutex<Tables>>);

impl Service for MockService {
    type Request = Request<'static>;
    type Response = Response;
    type Exception = ExceptionCode;
    type Future = future::Ready<Result<Response, ExceptionCode>>;

    fn call(&self, request: Self::Request) -> Self::Future {
        let mut tables = self.0.lock().unwrap_or_else(PoisonError::into_inner);

        let response = match request {
            Request::ReadCoils(address, quantity) => {
                read(&tables.coils, address, quantity).map(Response::ReadCoils)
            }
            Request::ReadDiscreteInputs(address, quantity) => {
                read(&tables.discrete_inputs, address, quantity).map(Response::ReadDiscreteInputs)
            }
            Request::ReadInputRegisters(address, quantity) => {
                read(&tables.input_registers, address, quantity).map(Response::ReadInputRegisters)
            }
            Request::ReadHoldingRegisters(address, quantity) => {
                read(&tables.holding_registers, address, quantity)
                    .map(Response::ReadHoldingRegisters)
            }
            Request::WriteSingleCoil(address, value) => {
                tables.coils.insert(address, value);
                Ok(Response::WriteSingleCoil(address, value))
            }
            Request::WriteSingleRegister(address, word) => {
                tables.holding_registers.insert(address, word);
                Ok(Response::WriteSingleRegister(address, word))
            }
            Request::WriteMultipleRegisters(address, words) => {
                insert_words(&mut tables.holding_registers, address, &words);
                let quantity = u16::try_from(words.len()).unwrap();
                Ok(Response::WriteMultipleRegisters(address, quantity))
            }
            _ => Err(ExceptionCode::IllegalFunction),
        };

        future::ready(response)
    }
}

fn insert_words(table: &mut HashMap<u16, u16>, address: u16, words: &[u16]) {
    for (offset, word) in (0..).zip(words) {
        table.insert(address + offset, *word);
    }
}

fn read<T: Copy>(
    table: &HashMap<u16, T>,
    address: u16,
    quantity: u16,
) -> Result<Vec<T>, ExceptionCode> {
    (address..address + quantity)
        .map(|address| table.get(&address).copied())
        .collect::<Option<_>>()
        .ok_or(ExceptionCode::IllegalDataAddress)
}

/// A TCP server that accepts connections and never answers on them: a device that
/// hangs. Dropping it stops the server.
pub struct SilentModbusServer {
    addr: SocketAddr,
    _task: AbortOnDrop,
}

impl SilentModbusServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let task = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                open.push(stream);
            }
        });

        Self {
            addr,
            _task: AbortOnDrop(task),
        }
    }

    /// The address of this server, as a bridge connects to it.
    pub fn address(&self) -> sorg_common::ModbusServerAddress {
        server_address(self.addr)
    }
}

fn server_address(addr: SocketAddr) -> sorg_common::ModbusServerAddress {
    sorg_common::ModbusServerAddress {
        host: addr.ip().to_string(),
        port: addr.port(),
    }
}
