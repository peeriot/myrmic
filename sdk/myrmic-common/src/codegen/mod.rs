//! Shared code generation logic for cell macros and build tools.
//!
//! This module contains the parsing and code generation logic that is shared
//! between `myrmic-sdk-macros` (compile-time proc macro) and `myrmic-build`
//! (build-time library).

pub use parse::{parse_cell_def, strip_cell_attrs};

use syn::{Expr, ExprLit, ImplItemFn, Lit, Type};

pub mod bridge_api;
pub mod cell_api;
pub mod generate;
pub mod status;
pub mod template;

mod parse;

/// Parses a `state_buf` / `arg_buf` value expression into a byte count. Accepts
/// an integer literal (`1024`) or a string literal holding an integer (`"1024"`).
pub fn parse_buf_size(expr: &Expr) -> syn::Result<usize> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(i), ..
        }) => i.base10_parse(),
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => s
            .value()
            .parse()
            .map_err(|err| syn::Error::new_spanned(s, format!("invalid buffer size: {err}"))),
        other => Err(syn::Error::new_spanned(
            other,
            "buffer size must be an integer literal or a string literal",
        )),
    }
}

pub struct CellDefinition<'a> {
    pub struct_ty: &'a Type,
    pub init_method: Option<InitMethod<'a>>,
    pub commands: Vec<CommandMethod<'a>>,
    pub event_handlers: Vec<EventHandlerMethod<'a>>,
    pub periodic_methods: Vec<PeriodicMethod<'a>>,
}

pub struct InitMethod<'a> {
    pub method: &'a ImplItemFn,
    pub returns_result: bool,
}

pub struct EventHandlerMethod<'a> {
    pub method: &'a ImplItemFn,
    /// `None` = no receiver, `Some(false)` = `&self`, `Some(true)` = `&mut self`.
    pub receiver: Option<bool>,
    /// Optional `arg_buf = ...` override on the `#[event_handler(...)]` attribute.
    /// `None` means use the default buffer size.
    pub arg_buf: Option<usize>,
}

pub struct PeriodicMethod<'a> {
    pub method: &'a ImplItemFn,
    /// `None` = no receiver, `Some(false)` = `&self`, `Some(true)` = `&mut self`.
    pub receiver: Option<bool>,
    /// Period parsed from the `every` attribute.
    pub period: std::time::Duration,
    /// Whether the timer waits for a tick handler to finish before scheduling the next period.
    pub fixed_delay: bool,
}

pub struct CommandMethod<'a> {
    pub method: &'a ImplItemFn,
    /// `None` = no receiver, `Some(false)` = `&self`, `Some(true)` = `&mut self`.
    pub receiver: Option<bool>,
    pub has_args: bool,
    /// Whether the user's function returns `Result<T>` (vs a plain type).
    pub returns_result: bool,
    /// The inner return type (`T` from `Result<T>`, or the plain type itself).
    /// `None` when the command returns `()` or `Result<()>`.
    pub return_ty: Option<&'a Type>,
    /// Optional `arg_buf = ...` override on the `#[command(...)]` attribute.
    /// `None` means use the default buffer size.
    pub arg_buf: Option<usize>,
}
