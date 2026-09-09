//! The `#[esp_firmware::main]` entry-point attribute.
//!
//! Expands your setup function into the entry point `#[esp_rtos::main]` would
//! produce — reached only through `esp_firmware`'s re-exports, so the firmware
//! crate depends on nothing but `esp-firmware` — that runs the firmware's boot
//! sequence, binds whatever you asked for, and then starts whatever is left on
//! the board.
//!
//! The body is spliced in rather than called, and a `Peripherals` parameter
//! names the binding `esp_hal::init` produces rather than a copy of it. Both
//! follow from the same fact: claiming hardware moves individual fields out,
//! and a partially-moved `Peripherals` can neither cross a function boundary
//! nor be re-bound — but it can stay in scope. The boot sequence takes `PSRAM`,
//! `TIMG0` and `FROM_CPU_INTR0` that way, `board!` takes the board's parts, and
//! whatever is left is still there in the body.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{FnArg, ItemFn, Pat, PatIdent, Type};

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

    Ok(bindings)
}

fn expand(f: &ItemFn) -> syn::Result<TokenStream2> {
    let bindings = bindings(f)?;
    let wants = |want| bindings.iter().any(|(w, ..)| *w == want);

    // `Peripherals` without a `Board` is the manual form: the caller claims
    // what it wants, builds the board and calls `start`.
    let manual = wants(Want::Peripherals) && !wants(Want::Board);

    let body = &f.block;
    let attrs = &f.attrs;

    // The peripherals are bound under the caller's name when asked for. The
    // boot sequence and `board!` move fields out of that binding, and the
    // spliced body still reaches what they left — which a fresh `let` after
    // those moves could never do.
    let (periph, periph_let) = match bindings.iter().find(|(w, ..)| *w == Want::Peripherals) {
        Some((_, pat, ty)) => {
            let Pat::Ident(PatIdent {
                by_ref: None,
                subpat: None,
                ident,
                ..
            }) = pat
            else {
                return Err(syn::Error::new_spanned(
                    pat,
                    "bind `Peripherals` to a plain name: the boot sequence and `board!` move \
                     fields out of it, which only works on a local binding",
                ));
            };
            (quote! { #ident }, quote! { let #pat: #ty })
        }
        None => (
            quote! { __esp_fw_peripherals },
            quote! { let mut __esp_fw_peripherals },
        ),
    };

    let build_board = (!manual).then(|| {
        quote! { let mut __esp_fw_board = ::esp_firmware::board!(#periph); }
    });
    let start = (!manual).then(|| {
        quote! { ::esp_firmware::start(__esp_fw_board, __esp_fw_spawner); }
    });

    // The declared type is kept on the binding: it type-checks the parameter at
    // the point the user wrote it, and it keeps the `use` that names it live.
    let lets = bindings.iter().filter_map(|(want, pat, ty)| match want {
        Want::Board => Some(quote! { let #pat: #ty = &mut __esp_fw_board; }),
        Want::Network => Some(quote! { let #pat: #ty = ::esp_firmware::network(); }),
        Want::Spawner => Some(quote! { let #pat: #ty = __esp_fw_spawner; }),
        // Bound where `esp_hal::init` runs, above.
        Want::Peripherals => None,
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
                #periph_let =
                    ::esp_firmware::esp_hal::init(::esp_firmware::esp_hal::Config::default());

                // The logger goes up before the heap so `setup_heap!`'s boot memory
                // summary (region sizes + low-heap warning) reaches the console.
                ::esp_firmware::__reexports::esp_common::esp_println::logger::init_logger_from_env();
                ::esp_firmware::__reexports::esp_common::esp_heap::setup_heap!(#periph);

                ::esp_firmware::__reexports::log::info!("Init!");
                ::esp_firmware::__boot_early();

                let __esp_fw_timg0 = ::esp_firmware::esp_hal::timer::timg::TimerGroup::new(
                    #periph.TIMG0,
                );
                ::esp_firmware::esp_rtos::start(
                    __esp_fw_timg0.timer0,
                    #periph.FROM_CPU_INTR0,
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
