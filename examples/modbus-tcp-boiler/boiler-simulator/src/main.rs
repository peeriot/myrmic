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

const ADDRESS_TEMPERATURE: u16 = 0x0100;
const ADDRESS_BURNER: u16 = 0x0005;
const ADDRESS_SETPOINT: u16 = 0x0200;

const ROOM_TEMPERATURE: f32 = 15.0;
const HEATING_PER_SECOND: f32 = 1.0;
const COOLING_PER_SECOND: f32 = 0.3;
const TICK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy)]
struct Boiler {
    temperature: f32,
    burner: bool,
    setpoint: i16,
}

impl Boiler {
    fn tick(self) -> Self {
        let seconds = TICK.as_secs_f32();
        let temperature = if self.burner {
            self.temperature + HEATING_PER_SECOND * seconds
        } else {
            (self.temperature - COOLING_PER_SECOND * seconds).max(ROOM_TEMPERATURE)
        };
        Self {
            temperature,
            ..self
        }
    }

    fn call(self, request: Request<'_>) -> Result<(Self, Response), ExceptionCode> {
        let answer = match request {
            Request::ReadInputRegisters(ADDRESS_TEMPERATURE, 2) => {
                log::debug!("Read temperature: {:.1} °C", self.temperature);
                let bits = self.temperature.to_bits();
                let words = vec![(bits >> 16) as u16, bits as u16];
                (self, Response::ReadInputRegisters(words))
            }
            Request::ReadCoils(ADDRESS_BURNER, 1) => {
                log::debug!("Read burner: {}", self.burner);
                (self, Response::ReadCoils(vec![self.burner]))
            }
            Request::WriteSingleCoil(ADDRESS_BURNER, on) => {
                log::debug!("Write burner: {on}");
                let boiler = Self { burner: on, ..self };
                (boiler, Response::WriteSingleCoil(ADDRESS_BURNER, on))
            }
            Request::ReadHoldingRegisters(ADDRESS_SETPOINT, 1) => {
                log::debug!("Read setpoint: {} °C", self.setpoint);
                let words = vec![self.setpoint as u16];
                (self, Response::ReadHoldingRegisters(words))
            }
            Request::WriteSingleRegister(ADDRESS_SETPOINT, word) => {
                log::debug!("Write setpoint: {} °C", word as i16);
                let setpoint = word as i16;
                let boiler = Self { setpoint, ..self };
                let response = Response::WriteSingleRegister(ADDRESS_SETPOINT, word);
                (boiler, response)
            }
            request => {
                log::debug!("Refuse unmapped request: {request:?}");
                return Err(ExceptionCode::IllegalDataAddress);
            }
        };

        Ok(answer)
    }
}

fn log_changes(before: &Boiler, after: &Boiler) {
    if after.burner != before.burner {
        log::info!(
            "burner {} at {:.1} °C",
            if after.burner { "on" } else { "off" },
            after.temperature
        );
    }
    if after.setpoint != before.setpoint {
        log::info!("setpoint {} °C", after.setpoint);
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
        let mut boiler = self.0.lock().unwrap();
        let answer = boiler.call(request).map(|(next, response)| {
            log_changes(&boiler, &next);
            log::debug!("New boiler state:\n{next:#?}");
            *boiler = next;
            response
        });
        future::ready(answer)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

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
    log::debug!("Initial boiler state:\n{boiler_state:#?}");
    let boiler = Arc::new(Mutex::new(boiler_state));

    tokio::spawn({
        let boiler = boiler.clone();
        async move {
            let mut interval = tokio::time::interval(TICK);
            loop {
                interval.tick().await;
                let mut boiler = boiler.lock().unwrap();
                *boiler = boiler.tick();
            }
        }
    });

    let listener = TcpListener::bind(address).await?;
    log::info!("boiler listening on modbus-tcp://{address}");

    let service = BoilerService(boiler);
    let on_connected = |stream, socket_addr| {
        let service = service.clone();
        async move { accept_tcp_connection(stream, socket_addr, |_| Ok(Some(service.clone()))) }
    };
    Server::new(listener)
        .serve(&on_connected, |err| log::warn!("connection failed: {err}"))
        .await?;

    Ok(())
}
