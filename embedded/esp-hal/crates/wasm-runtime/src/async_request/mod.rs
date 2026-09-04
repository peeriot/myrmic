//! Async request

#[cfg(feature = "ble")]
pub(crate) mod ble;
#[cfg(feature = "ble")]
pub(crate) mod ble_task;
pub(crate) mod cell_host;
pub(crate) mod db;
pub(crate) mod gpio;
pub(crate) mod timers;
pub mod zenoh;

use alloc::boxed::Box;
use core::mem::MaybeUninit;
use core::ptr;
use core::ptr::NonNull;
use core::time::Duration;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use embassy_sync::signal::Signal;
use embassy_time::Timer;
use esp_radio_rtos_driver::queue::QueueHandle;
use portable_atomic::{AtomicPtr, Ordering};
use static_cell::StaticCell;
use wasm_runtime_macros::root_requests;

use crate::Pins;
use crate::async_request::cell_host::CellContext;
use crate::async_request::db::DbContext;
use crate::async_request::zenoh::ZenohContext;

#[cfg(feature = "ble")]
pub use ble::{Ble, BleRequest, BleResponse};
#[cfg(feature = "ble")]
pub use ble_task::ble_manager_task;
pub use cell_host::{
    CELL_MSG_CHANNEL, CellRequest, CellResponse, CommandHandledGuard, command_handled,
    reset_command_handled,
};
pub use db::{DbClient, DbClientRequest, DbClientResponse, DbRequest, DbResponse};
pub use gpio::{GpioRequest, GpioResponse};
pub use timers::timer_manager_task;
pub use zenoh::{ZenohRequest, ZenohResponse};

/// Signal used to send and receive requests
static ASYNC_REQUEST: Signal<CriticalSectionRawMutex, Request> = Signal::new();
/// Storage of the `esp-rtos` [`QueueHandle`] that sends responses back to the requester.
///
/// It's designed in a way so that `esp-rtos` can efficiently block the task of the requester
/// without spin-looping
/// This has to be initialized once by the request handler, and make sure that the companion
/// pointer, [`ASYNC_RESPONSE_PTR`] is initialized to a non-null value;
static ASYNC_RESPONSE_STORAGE: StaticCell<QueueHandle> = StaticCell::new();
/// Companion pointer to [`ASYNC_RESPONSE_STORAGE`] which allows to globally grab access to the
/// queue by the requester.
///
/// This might be null if not yet initialized by the request handler.
static ASYNC_RESPONSE_PTR: AtomicPtr<QueueHandle> = AtomicPtr::new(core::ptr::null_mut());

/// Context of the system used for handling WASM request
///
/// This usually includes hardware peripherals and communication stacks
#[derive(Debug)]
pub struct Context {
    /// GPIO pins
    pins: Pins,
    /// Context of the cell
    cell: CellContext,
    /// Context of the DB client
    db: DbContext,
    /// Context of the Zenoh client
    zenoh: ZenohContext,
    /// Context of the cell timers
    pub(crate) timers: timers::TimersContext,
}

/// Request Errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Generic error")]
    Generic,
    #[error("Timeout")]
    Timeout,
    #[error("Invalid argument")]
    InvalidArg,
    #[error("The operation requires an encrypted/authenticated link (ATT security error)")]
    RequiresSecurity,
    #[error("The link was lost while the operation was in flight")]
    Disconnected,
}

/// A trait that associates response types to requests, avoiding lot of boilerplate and manual
/// destructuring logic
pub trait TypedRequest {
    /// Response type
    type Response;
    /// Converts this typed request into the top-level [`Request`] enum
    fn into_request(self) -> Request;
    /// Extracts the response
    fn extract_response(resp: Response) -> Self::Response;
}

/// Internal helper used by the request macros to walk `Response` down to a nested response type.
///
/// Each `requests!` invocation provides one hop (`Response -> OuterResp -> InnerResp`); the
/// chain composes automatically. Not intended for direct use.
#[doc(hidden)]
pub trait ExtractFromResponse: Sized {
    fn extract_from_response(resp: Response) -> Self;
}

/// Sends a requests and efficiently block until a response is obtained
///
/// To be used by a thread executor
pub fn send_request_and_wait<R: TypedRequest>(req: R) -> R::Response {
    R::extract_response(send_request(req.into_request()))
}

root_requests! {
    TimeWait(Duration)                              => (),

    category CellHost(CellRequest)                  => CellResponse,
    category DB(DbRequest)                          => DbResponse,
    category Gpio(GpioRequest)                      => GpioResponse,
    category Zenoh(ZenohRequest)                    => ZenohResponse,
    #[cfg(feature = "ble")]
    category Ble(BleRequest)                        => BleResponse,
}

/// The result of a response where we are just interested in the potential error returned
pub type ResponseResult = Result<(), Error>;

impl TypedRequest for Request {
    type Response = Response;

    fn into_request(self) -> Request {
        self
    }

    fn extract_response(response: Response) -> Response {
        response
    }
}

impl Request {
    /// Executes the Request
    #[expect(clippy::missing_panics_doc, reason = "Internal logic panic")]
    pub async fn execute(self, ctx: &mut Context) -> Response {
        match self {
            Request::TimeWait(dur) => {
                #[expect(
                    clippy::unwrap_used,
                    reason = "can't see this failing (even at 1GHz this would take half a millenia to break)"
                )]
                Timer::after(dur.try_into().unwrap()).await;

                Response::TimeWait
            }
            Request::CellHost(cell_req) => {
                Response::CellHost(cell_host::execute_request(ctx, cell_req).await)
            }
            Request::DB(db_req) => Response::DB(db::execute_request(ctx, db_req).await),
            Request::Gpio(gpio_req) => Response::Gpio(gpio::execute_request(ctx, gpio_req).await),
            Request::Zenoh(zenoh_req) => {
                Response::Zenoh(zenoh::execute_request(ctx, zenoh_req).await)
            }
            #[cfg(feature = "ble")]
            Request::Ble(ble_req) => Response::Ble(ble::execute_request(ble_req).await),
        }
    }
}

/// Handles asynchronous requests
///
/// To be used by an interrupt executor
pub async fn request_handler(
    pins: Pins,
    db_requests: Sender<'static, CriticalSectionRawMutex, DbClientRequest, 1>,
    db_responses: Receiver<'static, CriticalSectionRawMutex, DbClientResponse, 1>,
    zenoh_requests: zenoh::RequestsSender,
    zenoh_responses: zenoh::ResponsesReceiver,
    liveliness_bumper: fn(),
) -> Response {
    let mut ctx = Context {
        pins,
        cell: CellContext::default(),
        db: DbContext {
            requests: db_requests,
            responses: db_responses,
        },
        zenoh: ZenohContext {
            requests: zenoh_requests,
            responses: zenoh_responses,
        },
        timers: timers::TimersContext {
            next_id: timers::TimerId(0),
        },
    };

    // Initialize the response queue
    let response_queue =
        ASYNC_RESPONSE_STORAGE.init(QueueHandle::new(1, size_of::<*mut Response>()));
    ASYNC_RESPONSE_PTR.store(ptr::from_mut(response_queue), Ordering::Release);

    loop {
        // This makes sure the liveliness for the requrest handler is fired at each iteration of the
        // loop (makes sure that the request handler is not hung)
        liveliness_bumper();

        // Wait for next request
        let next_request = ASYNC_REQUEST.wait().await;

        // Execute the request asynchronously and await its response
        let resp = next_request.execute(&mut ctx).await;
        // Reset so that we can use the signal for the next operation
        ASYNC_REQUEST.reset();

        // Unblock the low priority executor
        let boxed = Box::new(resp);
        let ptr: *mut Response = Box::into_raw(boxed);
        // safety: We made sure that the queue size was initialized with size_of::<*mut Response>
        unsafe {
            response_queue.send_to_back((&raw const ptr).cast(), None);
        }
    }
}

fn send_request(request: Request) -> Response {
    // Send the request
    ASYNC_REQUEST.signal(request);

    loop {
        let queue_ptr = ASYNC_RESPONSE_PTR.load(Ordering::Acquire);
        if queue_ptr.is_null() {
            riscv::asm::wfi();
        } else {
            // safety: We know the pointer is convertible to a reference
            #[expect(clippy::unwrap_used, reason = "Already checked if null")]
            let queue: &'static QueueHandle = unsafe { NonNull::new(queue_ptr).unwrap().as_ref() };
            let mut ptr_out = MaybeUninit::<*mut Response>::uninit();

            // safety: We made sure that the queue size was initialized with size_of::<*mut Response>
            unsafe {
                // Block forever (until we get a response)
                // safety: No timeout = always true
                assert!(queue.receive(ptr_out.as_mut_ptr().cast(), None));
            }
            // Reconstruct the Box and take ownership of it
            // safety: We initialized it with what we received from the queue
            let ptr = unsafe { ptr_out.assume_init() };
            // safety: We own the Box now and there's no other pathway that takes ownership of this object
            let resp = unsafe { Box::from_raw(ptr) };

            break *resp;
        }
    }
}
