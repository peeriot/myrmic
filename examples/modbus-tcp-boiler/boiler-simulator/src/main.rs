//! A simulated boiler that speaks Modbus TCP,
//! so the Modbus bridge can be tried without hardware.
//! It maps three values:
//!
//! | Table            | Address   | Value                                           |
//! |------------------|-----------|-------------------------------------------------|
//! | input registers  | 100, 101  | water temperature in °C, `f32`, high word first |
//! | coils            | 5         | burner on/off                                   |
//! | holding register | 200       | setpoint in °C, `i16`                           |
//!
//! While the burner is on the water heats up by 1 °C per second,
//! while it is off it cools down by 0.3 °C per second,
//! but not below the room it stands in.
//!
//! Usage: `boiler-sim [ADDRESS]`, listening on `127.0.0.1:5020` by default.

use std::{
    future,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::net::TcpListener;

use tokio_modbus::{
    ExceptionCode, Request, Response,
    server::{
        Service,
        tcp::{Server, accept_tcp_connection},
    },
};

const ADDRESS_TEMPERATURE: u16 = 100;
const ADDRESS_BURNER: u16 = 5;
const ADDRESS_SETPOINT: u16 = 200;

const ROOM_TEMPERATURE: f32 = 15.0;
const HEATING_PER_SECOND: f32 = 1.0;
const COOLING_PER_SECOND: f32 = 0.3;
const TICK: Duration = Duration::from_millis(100);

#[derive(Debug)]
struct Boiler {
    temperature: f32,
    burner: bool,
    setpoint: i16,
}

impl Boiler {
    fn tick(&mut self) {
        let seconds = TICK.as_secs_f32();
        self.temperature = if self.burner {
            self.temperature + HEATING_PER_SECOND * seconds
        } else {
            (self.temperature - COOLING_PER_SECOND * seconds).max(ROOM_TEMPERATURE)
        };
    }

    fn call(&mut self, request: Request<'_>) -> Result<Response, ExceptionCode> {
        let response = match request {
            Request::ReadInputRegisters(ADDRESS_TEMPERATURE, 2) => {
                log::debug!("Read the boiler temperature");
                let bits = self.temperature.to_bits();
                let words = vec![(bits >> 16) as u16, bits as u16];
                Response::ReadInputRegisters(words)
            }
            Request::ReadCoils(ADDRESS_BURNER, 1) => {
                log::debug!("Read the burner state");
                Response::ReadCoils(vec![self.burner])
            }
            Request::WriteSingleCoil(ADDRESS_BURNER, on) => {
                log::debug!("Set the burner state to {}", if on { "on" } else { "off" });
                if on != self.burner {
                    println!(
                        "burner {} at {:.1} °C",
                        if on { "on" } else { "off" },
                        self.temperature
                    );
                }
                self.burner = on;
                Response::WriteSingleCoil(ADDRESS_BURNER, on)
            }
            Request::ReadHoldingRegisters(ADDRESS_SETPOINT, 1) => {
                log::debug!("Read the setpoint");
                Response::ReadHoldingRegisters(vec![self.setpoint as u16])
            }
            Request::WriteSingleRegister(ADDRESS_SETPOINT, word) => {
                let new_value = word as i16;
                log::debug!("Write the setpoint to {new_value}");
                self.setpoint = new_value;
                println!("setpoint {} °C", self.setpoint);
                Response::WriteSingleRegister(ADDRESS_SETPOINT, word)
            }
            // Anything else, like a device that does not map the address.
            _ => {
                log::warn!("Invalid request: {request:?}");
                return Err(ExceptionCode::IllegalDataAddress);
            }
        };

        log::debug!("New boiler state:\n {self:#?}");

        Ok(response)
    }
}

#[derive(Clone)]
struct BoilerService(Arc<Mutex<Boiler>>);

impl Service for BoilerService {
    type Request = Request<'static>;
    type Response = Response;
    type Exception = ExceptionCode;
    type Future = future::Ready<Result<Response, ExceptionCode>>;

    fn call(&self, request: Self::Request) -> Self::Future {
        future::ready(self.0.lock().unwrap().call(request))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let address: SocketAddr = std::env::args()
        .nth(1)
        .as_deref()
        .unwrap_or("127.0.0.1:5020")
        .parse()?;

    let boiler_state = Boiler {
        temperature: 20.0,
        burner: false,
        setpoint: 60,
    };
    log::debug!("Initial boiler state:\n {boiler_state:#?}");

    let boiler = Arc::new(Mutex::new(boiler_state));

    tokio::spawn({
        let boiler = boiler.clone();
        async move {
            let mut interval = tokio::time::interval(TICK);
            loop {
                interval.tick().await;
                boiler.lock().unwrap().tick();
            }
        }
    });

    let listener = TcpListener::bind(address).await?;
    println!("Modbus TCP boiler simulator listening on {address}");

    let service = BoilerService(boiler);
    let on_connected = |stream, socket_addr| {
        let service = service.clone();
        async move { accept_tcp_connection(stream, socket_addr, |_| Ok(Some(service.clone()))) }
    };

    Server::new(listener)
        .serve(&on_connected, |err| eprintln!("connection failed: {err}"))
        .await?;

    Ok(())
}
