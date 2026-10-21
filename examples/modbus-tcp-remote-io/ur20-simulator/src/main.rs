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

const ADDRESS_HEAT_BUTTON: u16 = 0x0000;
const ADDRESS_PT100: u16 = 0x0001;
const ADDRESS_FAN: u16 = 0x8000;
const ADDRESS_HEATER: u16 = 0x8001;
const ADDRESS_MODULE_LIST: u16 = 0x2A00;
const ADDRESS_HEAT_BUTTON_INPUT_DELAY: u16 = 0xC000;
const ADDRESS_PT100_MEASUREMENT_RANGE: u16 = 0xC101;
const ADDRESS_PT100_CONNECTION_TYPE: u16 = 0xC102;

const INPUT_DELAY_3_MS: u16 = 2;
const MEASUREMENT_RANGE_DISABLED: u16 = 18;
const CONNECTION_TYPE_TWO_WIRE: u16 = 0;

const UR20_4DI_P: u32 = 0x0009_1F84;
const UR20_4AI_RTD_DIAG: u32 = 0x0406_1544;
const UR20_4DO_P_2A: u32 = 0x0105_2FA0;
const MODULES: [u32; 3] = [UR20_4DI_P, UR20_4AI_RTD_DIAG, UR20_4DO_P_2A];

const ROOM_TEMPERATURE: f32 = 22.0;
const HAND_TEMPERATURE: f32 = 34.0;
const HEATING_PER_SECOND: f32 = 0.4;
const LOSS_PER_SECOND: f32 = 0.01;
const FAN_COOLING_PER_SECOND: f32 = 0.12;
const GRIP_PER_SECOND: f32 = 0.05;
const HEAT_BUTTON_PRESS: Duration = Duration::from_millis(150);
const TICK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy)]
struct Simulation {
    temperature: f32,
    hand: bool,
    fan: bool,
    heater: bool,
    heat_button_pressed_for: Duration,
    heat_button_input_delay: u16,
    pt100_measurement_range: u16,
    pt100_connection_type: u16,
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
        if self.heater {
            change += HEATING_PER_SECOND;
        }
        Self {
            temperature: self.temperature + change * seconds,
            heat_button_pressed_for: self.heat_button_pressed_for.saturating_sub(TICK),
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
            Request::ReadDiscreteInputs(ADDRESS_HEAT_BUTTON, 1) => {
                let pressed = !self.heat_button_pressed_for.is_zero();
                log::debug!("Read heat button: {pressed}");
                (self, Response::ReadDiscreteInputs(vec![pressed]))
            }
            Request::ReadHoldingRegisters(ADDRESS_HEAT_BUTTON_INPUT_DELAY, 1) => {
                let delay = self.heat_button_input_delay;
                log::debug!("Read heat button input delay: {delay}");
                (self, Response::ReadHoldingRegisters(vec![delay]))
            }
            Request::WriteSingleRegister(ADDRESS_HEAT_BUTTON_INPUT_DELAY, delay) => {
                log::debug!("Write heat button input delay: {delay}");
                let simulation = Self {
                    heat_button_input_delay: delay,
                    ..self
                };
                let response =
                    Response::WriteSingleRegister(ADDRESS_HEAT_BUTTON_INPUT_DELAY, delay);
                (simulation, response)
            }
            Request::ReadHoldingRegisters(ADDRESS_PT100_MEASUREMENT_RANGE, 1) => {
                let range = self.pt100_measurement_range;
                log::debug!("Read PT100 measurement range: {range}");
                (self, Response::ReadHoldingRegisters(vec![range]))
            }
            Request::ReadHoldingRegisters(ADDRESS_PT100_CONNECTION_TYPE, 1) => {
                let connection = self.pt100_connection_type;
                log::debug!("Read PT100 connection type: {connection}");
                (self, Response::ReadHoldingRegisters(vec![connection]))
            }
            Request::WriteSingleRegister(ADDRESS_PT100_MEASUREMENT_RANGE, range) => {
                log::debug!("Write PT100 measurement range: {range}");
                let simulation = Self {
                    pt100_measurement_range: range,
                    ..self
                };
                let response =
                    Response::WriteSingleRegister(ADDRESS_PT100_MEASUREMENT_RANGE, range);
                (simulation, response)
            }
            Request::WriteSingleRegister(ADDRESS_PT100_CONNECTION_TYPE, connection) => {
                log::debug!("Write PT100 connection type: {connection}");
                let simulation = Self {
                    pt100_connection_type: connection,
                    ..self
                };
                let response =
                    Response::WriteSingleRegister(ADDRESS_PT100_CONNECTION_TYPE, connection);
                (simulation, response)
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
            Request::WriteSingleCoil(ADDRESS_HEATER, on) => {
                log::debug!("Write heater: {on}");
                let simulation = Self { heater: on, ..self };
                (simulation, Response::WriteSingleCoil(ADDRESS_HEATER, on))
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
    if after.heater != before.heater {
        log::info!(
            "heater {} at {:.1} °C",
            if after.heater { "on" } else { "off" },
            after.temperature
        );
    }
    if after.heat_button_input_delay != before.heat_button_input_delay {
        log::info!(
            "heat button input delay set to {}",
            after.heat_button_input_delay
        );
    }
    if after.pt100_measurement_range != before.pt100_measurement_range {
        log::info!(
            "PT100 measurement range set to {}",
            after.pt100_measurement_range
        );
    }
    if after.pt100_connection_type != before.pt100_connection_type {
        log::info!(
            "PT100 connection type set to {}",
            after.pt100_connection_type
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
        heater: false,
        heat_button_pressed_for: Duration::ZERO,
        heat_button_input_delay: INPUT_DELAY_3_MS,
        pt100_measurement_range: MEASUREMENT_RANGE_DISABLED,
        pt100_connection_type: CONNECTION_TYPE_TWO_WIRE,
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
                if line.trim() == "b" {
                    simulation.heat_button_pressed_for = HEAT_BUTTON_PRESS;
                    log::info!("heat button pressed at {:.1} °C", simulation.temperature);
                    continue;
                }
                if line.trim() != "h" {
                    log::warn!(
                        "type `h` to take the PT100 in the hand or let it go, \
                         `b` to press the heat button"
                    );
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
