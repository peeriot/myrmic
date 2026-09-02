//! The `#[esp_firmware::main]` entry-point attribute.
//!
//! Expands your setup function into the entry point `#[esp_rtos::main]` would
//! produce — reached only through `esp_firmware`'s re-exports, so the firmware
//! crate depends on nothing but `esp-firmware` — that runs the firmware's boot
//! sequence, binds whatever you asked for, and then starts whatever is left on
//! the board.
//!
//! The body is spliced in rather than called. That is what lets a setup
//! function take `Peripherals` and claim hardware from it: claiming moves
//! individual fields out, and a partially-moved `Peripherals` cannot cross a
//! function boundary — but it can stay in scope.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{FnArg, ItemFn, Pat, Type};

/// What a setup-function parameter is asking for. Chosen by the type's final
/// path segment, so `esp_firmware::Board` and a plain `Board` both work.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Want {
    Board,
    Network,
    Spawner,
    Peripherals,
}

impl Want {
    fn from_type(ty: &Type) -> Option<Self> {
        let path = match ty {
            // `&mut Board` and a bare `Board` alike.
            Type::Reference(r) => match &*r.elem {
                Type::Path(p) => &p.path,
                _ => return None,
            },
            Type::Path(p) => &p.path,
            _ => return None,
        };
        match path.segments.last()?.ident.to_string().as_str() {
            "Board" => Some(Self::Board),
            "Network" => Some(Self::Network),
            "Spawner" => Some(Self::Spawner),
            "Peripherals" => Some(Self::Peripherals),
            _ => None,
        }
    }
}

/// Marks the firmware entry point. See the `esp_firmware::main` docs.
#[proc_macro_attribute]
pub fn main(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = TokenStream2::from(args);
    if !args.is_empty() {
        return syn::Error::new_spanned(args, "`#[esp_firmware::main]` takes no arguments")
            .to_compile_error()
            .into();
    }

    let f = syn::parse_macro_input!(item as ItemFn);
    expand(&f)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Checks the signature and resolves what each parameter asks for.
fn bindings(f: &ItemFn) -> syn::Result<Vec<(Want, &Pat, &Type)>> {
    if f.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &f.sig,
            "the setup function must be `async` — it runs on the main executor",
        ));
    }
    if !f.sig.generics.params.is_empty() || f.sig.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &f.sig,
            "the setup function must not be generic",
        ));
    }

    let mut bindings: Vec<(Want, &Pat, &Type)> = Vec::new();
    for arg in &f.sig.inputs {
        let FnArg::Typed(pat) = arg else {
            return Err(syn::Error::new_spanned(
                arg,
                "the setup function must be a free function, not a method",
            ));
        };
        let Some(want) = Want::from_type(&pat.ty) else {
            return Err(syn::Error::new_spanned(
                &pat.ty,
                "unsupported setup parameter: expected `&mut Board`, `Network`, `Spawner`, \
                 or `Peripherals`",
            ));
        };
        if bindings.iter().any(|(seen, ..)| *seen == want) {
            return Err(syn::Error::new_spanned(
                &pat.ty,
                "each setup parameter may appear only once",
            ));
        }
        bindings.push((want, &pat.pat, &pat.ty));
    }

    let manual = bindings.iter().any(|(w, ..)| *w == Want::Peripherals);
    if manual
        && bindings
            .iter()
            .any(|(w, ..)| matches!(w, Want::Board | Want::Network))
    {
        return Err(syn::Error::new_spanned(
            &f.sig,
            "a setup function taking `Peripherals` claims the hardware itself, so it cannot \
             also take `Board` or `Network`; build the board with `esp_firmware::board!` and \
             call `esp_firmware::network()`",
        ));
    }

    Ok(bindings)
}

fn expand(f: &ItemFn) -> syn::Result<TokenStream2> {
    let bindings = bindings(f)?;
    let manual = bindings.iter().any(|(w, ..)| *w == Want::Peripherals);

    let body = &f.block;
    let attrs = &f.attrs;

    // The board is only built when the setup function did not take the
    // peripherals for itself.
    let build_board = (!manual).then(|| {
        quote! { let mut __esp_fw_board = ::esp_firmware::board!(__esp_fw_peripherals); }
    });
    let start = (!manual).then(|| {
        quote! { ::esp_firmware::start(__esp_fw_board, __esp_fw_spawner); }
    });

    // The declared type is kept on the binding: it type-checks the parameter at
    // the point the user wrote it, and it keeps the `use` that names it live.
    let lets = bindings.iter().map(|(want, pat, ty)| match want {
        Want::Board => quote! { let #pat: #ty = &mut __esp_fw_board; },
        Want::Network => quote! { let #pat: #ty = ::esp_firmware::network(); },
        Want::Spawner => quote! { let #pat: #ty = __esp_fw_spawner; },
        Want::Peripherals => quote! { let #pat: #ty = __esp_fw_peripherals; },
    });

    // This is what `#[esp_rtos::main]` expands to, spelled with paths through
    // `esp_firmware`'s re-exports. That macro hard-codes `::esp_rtos`,
    // `::embassy_executor` and `esp_hal`, which would force every firmware
    // crate to depend on all three directly.
    Ok(quote! {
        ::esp_firmware::__reexports::esp_common::esp_bootloader_esp_idf::esp_app_desc!();

        #[doc(hidden)]
        mod __esp_firmware_entry {
            use super::*;

            #(#attrs)*
            #[::esp_firmware::embassy_executor::task(
                embassy_executor = ::esp_firmware::embassy_executor
            )]
            async fn __esp_firmware_main(
                __esp_fw_spawner: ::esp_firmware::embassy_executor::Spawner,
            ) {
                #[allow(unused_mut)]
                let mut __esp_fw_peripherals =
                    ::esp_firmware::esp_hal::init(::esp_firmware::esp_hal::Config::default());

                // The logger goes up before the heap so `setup_heap!`'s boot memory
                // summary (region sizes + low-heap warning) reaches the console.
                ::esp_firmware::__reexports::esp_common::esp_println::logger::init_logger_from_env();
                ::esp_firmware::__reexports::esp_common::esp_heap::setup_heap!(__esp_fw_peripherals);

                ::esp_firmware::__reexports::log::info!("Init!");
                ::esp_firmware::__boot_early();

                let __esp_fw_timg0 = ::esp_firmware::esp_hal::timer::timg::TimerGroup::new(
                    __esp_fw_peripherals.TIMG0,
                );
                ::esp_firmware::esp_rtos::start(
                    __esp_fw_timg0.timer0,
                    __esp_fw_peripherals.FROM_CPU_INTR0,
                );
                ::esp_firmware::__boot_late();

                #build_board

                {
                    #(#lets)*
                    #body
                }

                #start
            }

            unsafe fn __make_static<T>(t: &mut T) -> &'static mut T {
                ::core::mem::transmute(t)
            }

            #[::esp_firmware::esp_hal::__macro_implementation::__entry]
            fn main() -> ! {
                let mut executor = ::esp_firmware::esp_rtos::embassy::Executor::new();
                let executor = unsafe { __make_static(&mut executor) };
                executor.run(|spawner| {
                    spawner.spawn(__esp_firmware_main(spawner).unwrap());
                })
            }
        }
    })
}
