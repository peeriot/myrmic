//! GPIO async request

use esp_hal::gpio::{Event, Level};
use wasm_runtime_macros::requests;

use crate::async_request::{Context, Error, Request, Response, ResponseResult};
use crate::imports::gpio::Edge;

requests! {
    wrap(GpioRequest => Request::Gpio),
    unwrap(Response::Gpio => GpioResponse);

    IsPinSupported { pin: usize } => bool,
    Set { pin: usize, level: Level }            => ResponseResult,
    Read { pin: usize }                         => Result<Level, Error>,
    WaitForLevel { pin: usize, level: Level }   => ResponseResult,
    WaitForEdge { pin: usize, edge: Edge }      => ResponseResult,
}

/// Executes the cell host async request
pub(crate) async fn execute_request(ctx: &mut Context, req: GpioRequest) -> GpioResponse {
    log::trace!("[async req][Gpio] Received Request {req:?}");

    match req {
        GpioRequest::IsPinSupported { pin } => {
            GpioResponse::IsPinSupported(matches!(ctx.pins.get(pin), Some(Some(_))))
        }
        GpioRequest::Set { pin, level } => {
            if let Some(Some(pin)) = ctx.pins.get_mut(pin) {
                match level {
                    Level::Low => pin.set_low(),
                    Level::High => pin.set_high(),
                }

                GpioResponse::Set(Ok(()))
            } else {
                GpioResponse::Set(Err(Error::Generic))
            }
        }
        GpioRequest::Read { pin } => {
            if let Some(Some(pin)) = ctx.pins.get(pin) {
                GpioResponse::Read(Ok(pin.is_high().into()))
            } else {
                GpioResponse::Read(Err(Error::Generic))
            }
        }
        GpioRequest::WaitForLevel { pin, level } => {
            if let Some(Some(pin)) = ctx.pins.get_mut(pin) {
                match level {
                    Level::Low => pin.wait_for(Event::LowLevel).await,
                    Level::High => pin.wait_for(Event::HighLevel).await,
                }
                GpioResponse::WaitForLevel(Ok(()))
            } else {
                GpioResponse::WaitForLevel(Err(Error::Generic))
            }
        }
        GpioRequest::WaitForEdge { pin, edge } => {
            if let Some(Some(pin)) = ctx.pins.get_mut(pin) {
                match edge {
                    Edge::Rising => pin.wait_for(Event::RisingEdge).await,
                    Edge::Falling => pin.wait_for(Event::FallingEdge).await,
                    Edge::Any => pin.wait_for(Event::AnyEdge).await,
                }
                GpioResponse::WaitForEdge(Ok(()))
            } else {
                GpioResponse::WaitForEdge(Err(Error::Generic))
            }
        }
    }
}
