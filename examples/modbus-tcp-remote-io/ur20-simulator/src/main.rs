use std::{
    future,
    io::BufRead,
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

const ADDRESS_PT100: u16 = 0x0001;
const ADDRESS_FAN: u16 = 0x8000;
const ADDRESS_MODULE_LIST: u16 = 0x2A00;

const UR20_4DI_P: u32 = 0x0009_1F84;
const UR20_4AI_RTD_DIAG: u32 = 0x0406_1544;
const UR20_4DO_P_2A: u32 = 0x0105_2FA0;
const MODULES: [u32; 3] = [UR20_4DI_P, UR20_4AI_RTD_DIAG, UR20_4DO_P_2A];

const ROOM_TEMPERATURE: f32 = 22.0;
const HAND_TEMPERATURE: f32 = 34.0;
const LOSS_PER_SECOND: f32 = 0.01;
const FAN_COOLING_PER_SECOND: f32 = 0.12;
const GRIP_PER_SECOND: f32 = 0.05;
const TICK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy)]
struct Simulation {
    temperature: f32,
    hand: bool,
    fan: bool,
}

impl Simulation {
    fn tick(self) -> Self {
        let seconds = TICK.as_secs_f32();
        let cooling = if self.fan {
            LOSS_PER_SECOND + FAN_COOLING_PER_SECOND
        } else {
            LOSS_PER_SECOND
        };
        let mut change = cooling * (ROOM_TEMPERATURE - self.temperature);
        if self.hand {
            change += GRIP_PER_SECOND * (HAND_TEMPERATURE - self.temperature);
        }
        Self {
            temperature: self.temperature + change * seconds,
            ..self
        }
    }

    fn call(self, request: Request<'_>) -> Result<(Self, Response), ExceptionCode> {
        let answer = match request {
            Request::ReadInputRegisters(ADDRESS_PT100, 1) => {
                log::debug!("Read PT100: {:.1} °C", self.temperature);
                let tenths = (self.temperature * 10.0).round() as i16;
                (self, Response::ReadInputRegisters(vec![tenths as u16]))
            }
            Request::ReadHoldingRegisters(address, count) => {
                let Some(words) = module_list(address, count) else {
                    log::debug!("Refuse read outside the module list: {address:#06x}");
                    return Err(ExceptionCode::IllegalDataAddress);
                };
                log::debug!("Read module list: {words:04x?}");
                (self, Response::ReadHoldingRegisters(words))
            }
            Request::WriteSingleCoil(ADDRESS_FAN, on) => {
                log::debug!("Write fan: {on}");
                let simulation = Self { fan: on, ..self };
                (simulation, Response::WriteSingleCoil(ADDRESS_FAN, on))
            }
            request => {
                log::debug!("Refuse unmapped request: {request:?}");
                return Err(ExceptionCode::IllegalDataAddress);
            }
        };

        Ok(answer)
    }
}

fn module_list(address: u16, count: u16) -> Option<Vec<u16>> {
    let words: Vec<u16> = MODULES
        .iter()
        .flat_map(|id| [(id >> 16) as u16, *id as u16])
        .collect();
    let start = usize::from(address.checked_sub(ADDRESS_MODULE_LIST)?);
    words
        .get(start..start + usize::from(count))
        .map(<[u16]>::to_vec)
}

fn log_changes(before: &Simulation, after: &Simulation) {
    if after.fan != before.fan {
        log::info!(
            "fan {} at {:.1} °C",
            if after.fan { "on" } else { "off" },
            after.temperature
        );
    }
}

#[derive(Clone)]
struct CouplerService(Arc<Mutex<Simulation>>);

impl Service for CouplerService {
    type Request = Request<'static>;
    type Response = Response;
    type Exception = ExceptionCode;
    type Future = future::Ready<Result<Response, ExceptionCode>>;

    fn call(&self, request: Self::Request) -> Self::Future {
        let mut simulation = self.0.lock().unwrap();
        let answer = simulation.call(request).map(|(next, response)| {
            log_changes(&simulation, &next);
            log::debug!("New simulation state:\n{next:#?}");
            *simulation = next;
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

    let simulation_state = Simulation {
        temperature: ROOM_TEMPERATURE,
        hand: false,
        fan: false,
    };
    log::debug!("Initial simulation state:\n{simulation_state:#?}");
    let simulation = Arc::new(Mutex::new(simulation_state));

    tokio::spawn({
        let simulation = simulation.clone();
        async move {
            let mut interval = tokio::time::interval(TICK);
            loop {
                interval.tick().await;
                let mut simulation = simulation.lock().unwrap();
                *simulation = simulation.tick();
            }
        }
    });

    // Reading stdin blocks, so it gets a thread of its own.
    std::thread::spawn({
        let simulation = simulation.clone();
        move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                let mut simulation = simulation.lock().unwrap();
                if line.trim() != "h" {
                    log::warn!("type `h` to take the PT100 in the hand or let it go");
                    continue;
                }
                simulation.hand = !simulation.hand;
                log::info!(
                    "PT100 {} at {:.1} °C",
                    if simulation.hand {
                        "in the hand"
                    } else {
                        "let go"
                    },
                    simulation.temperature
                );
            }
        }
    });

    let listener = TcpListener::bind(address).await?;
    log::info!("UR20 coupler listening on modbus-tcp://{address}");

    let service = CouplerService(simulation);
    let on_connected = |stream, socket_addr| {
        let service = service.clone();
        async move { accept_tcp_connection(stream, socket_addr, |_| Ok(Some(service.clone()))) }
    };
    Server::new(listener)
        .serve(&on_connected, |err| log::warn!("connection failed: {err}"))
        .await?;

    Ok(())
}
