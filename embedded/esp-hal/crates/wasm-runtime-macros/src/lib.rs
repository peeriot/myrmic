//! Proc-macro helpers for `wasm-runtime`.

mod entry;
mod requests;
mod root_requests;

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

/// Attribute macro that:
/// - Renames the user function into a private `__<name>_impl` implementation (which doesn't
///   contain WAMR args like `exec_env`)
/// - Generates an `#[unsafe(no_mangle)] pub unsafe extern "C" fn <name>_host_function(exec_env, ...)`
///   wrapper that calls the private implementation, so that the user doesn't have to worry about
///   being verbose in the declaration of host function implementations
///
/// To use the attribute correctly, one has to provide a `crate::shutdown` method that is used to
/// check whether the host function should abort.
///
/// # Panics
///
/// The macro will return a failure if the user tries to pass a function that takes `self` as an
/// argument
#[proc_macro_attribute]
pub fn host_function(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut user_fn = parse_macro_input!(item as ItemFn);

    let orig_name = user_fn.sig.ident.clone();
    let impl_name = format_ident!("__{}_impl", orig_name);
    let exported_name = format_ident!("{}_host_function", orig_name);

    // Rename impl
    user_fn.sig.ident = impl_name.clone();
    user_fn.vis = syn::parse_quote!();

    // Grab the signature parts we need for the wrapper:
    let inputs = user_fn.sig.inputs.clone();
    let output = user_fn.sig.output.clone();

    // Build arg list for calling impl: just identifiers/patterns from the user's args.
    let call_args = user_fn.sig.inputs.iter().map(|arg| match arg {
        syn::FnArg::Typed(pat_ty) => &*pat_ty.pat,
        #[expect(clippy::panic, reason = "Good to panic in proc-macros")]
        syn::FnArg::Receiver(_) => {
            panic!("host_function does not support methods (self)");
        }
    });

    let abort_return = match &output {
        syn::ReturnType::Default => quote! { return; },
        syn::ReturnType::Type(_, _) => quote! { return Default::default(); },
    };

    let expanded = quote! {
        #[expect(clippy::used_underscore_binding)]
        #user_fn

        #[unsafe(no_mangle)]
        #[expect(clippy::used_underscore_binding)]
        pub unsafe extern "C" fn #exported_name(
            exec_env: wamr_rust_sdk::sys::wasm_exec_env_t,
            #inputs
        ) #output {
            if crate::shutdown(exec_env) {
                #abort_return
            }

            #impl_name(#(#call_args),*)
        }
    };

    TokenStream::from(expanded)
}

/// Generates a typed request/response pair for a sub-category of async requests.
///
/// # Syntax
///
/// ```rust,ignore
/// requests! {
///     wrap(InnerRequest => OuterRequest::Variant),
///     unwrap(OuterResponse::Variant => InnerResponse);
///
///     // unit request, unit response
///     VariantName => (),
///     // unit request, typed response
///     VariantName => ResponseType,
///     // newtype request, any response
///     VariantName(FieldType) => ResponseType,
///     // named-fields request, any response
///     VariantName { field: FieldType, .. } => ResponseType,
///     // sub-category passthrough (no struct, no TypedRequest)
///     delegate SubName(SubRequest) => SubResponse,
/// }
/// ```
///
/// # What gets generated
///
/// - `pub enum InnerRequest { ... }` and `pub enum InnerResponse { ... }`
/// - `pub mod Variant { use super::*; pub struct VariantName; ... }` (one struct per non-delegate entry)
/// - Per non-delegate entry: `impl From<Variant::Name> for OuterRequest` and
///   `impl TypedRequest for Variant::Name` ([`TypedRequest`] only when [`OuterResponse`] == `Response`)
/// - `impl From<InnerRequest> for OuterRequest`
///
/// # [`TypedRequest`] generation
///
/// `TypedRequest` impls are only generated when the outer response in `unwrap(...)` is the
/// top-level `Response` type (i.e., the macro is at nesting level 2, directly under the root).
/// For deeper levels (e.g. `DbClientRequest` inside `DbRequest` inside `Request`), the outer
/// response is `DbResponse`, so [`TypedRequest`] is skipped - those variants are not individually
/// sendable via `send_request_and_wait`.
#[proc_macro]
pub fn requests(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as requests::RequestsInput);
    requests::codegen(input).into()
}

/// Generates the top-level `Request` and `Response` enums plus typed helper structs.
///
/// # Syntax
///
/// ```rust,ignore
/// root_requests! {
///     // flat newtype request, unit response
///     Name(T) => (),
///     // flat newtype request, typed response
///     Name(T) => ResponseType,
///     // flat named-fields request, any response
///     Name { field: T, .. } => ResponseType,
///     // sub-category passthrough (no struct, no TypedRequest)
///     category SubName(SubRequest) => SubResponse,
/// }
/// ```
///
/// # What gets generated
///
/// - `pub enum Request  { Name(T), ..., SubName(SubRequest), ... }`
/// - `pub enum Response { Name,    ..., SubName(SubResponse), ... }`
/// - Per non-category entry: `pub struct Name(pub T);` + `impl From<Name> for Request` +
///   `impl TypedRequest for Name`
///
/// Category entries add variants to both enums but produce no struct or [`TypedRequest`] - callers
/// use the structs emitted by the inner `requests!` invocation (e.g. `CellHost::GetSri`).
#[proc_macro]
pub fn root_requests(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as root_requests::RootRequestsInput);
    root_requests::codegen(input).into()
}
