use syn::{
    Attribute, FnArg, GenericArgument, ImplItem, ItemImpl, Meta, PathArguments, ReturnType, Type,
    punctuated::Punctuated, token::Comma,
};

use crate::codegen::{
    CellDefinition, CommandMethod, EventHandlerMethod, InitMethod, PeriodicMethod, parse_buf_size,
};

const KW_INIT: &str = "init";
const KW_COMMAND: &str = "command";
const KW_EVENT_HANDLER: &str = "event_handler";
const KW_PERIODIC: &str = "periodic";

pub fn parse_cell_def(item_impl: &'_ ItemImpl) -> syn::Result<CellDefinition<'_>> {
    let struct_ty = &*item_impl.self_ty;
    let mut init_method = None;
    let mut commands = Vec::new();
    let mut event_handlers = Vec::new();
    let mut periodic_methods = Vec::new();

    for item in &item_impl.items {
        let ImplItem::Fn(method) = item else { continue };

        let is_init = method.attrs.iter().any(|a| a.path().is_ident(KW_INIT));
        let is_command = method.attrs.iter().any(|a| a.path().is_ident(KW_COMMAND));
        let is_event_handler = method
            .attrs
            .iter()
            .any(|a| a.path().is_ident(KW_EVENT_HANDLER));
        let is_periodic = method.attrs.iter().any(|a| a.path().is_ident(KW_PERIODIC));

        if is_init && is_command {
            return Err(syn::Error::new_spanned(
                method,
                "a method cannot be both #[init] and #[command]",
            ));
        }
        if is_init && is_event_handler {
            return Err(syn::Error::new_spanned(
                method,
                "a method cannot be both #[init] and #[event_handler]",
            ));
        }
        if is_periodic && is_init {
            return Err(syn::Error::new_spanned(
                method,
                "a method cannot be both #[periodic] and #[init]",
            ));
        }
        if is_periodic && is_command {
            return Err(syn::Error::new_spanned(
                method,
                "a method cannot be both #[periodic] and #[command]",
            ));
        }
        if is_periodic && is_event_handler {
            return Err(syn::Error::new_spanned(
                method,
                "a method cannot be both #[periodic] and #[event_handler]",
            ));
        }

        if is_init {
            if init_method.is_some() {
                return Err(syn::Error::new_spanned(
                    method,
                    "A cell must have at most one method annotated with 'init'",
                ));
            }
            init_method = Some(collect_init(method, struct_ty)?);
        }
        if is_event_handler {
            event_handlers.push(collect_event_handler(method)?);
        }
        if is_command {
            commands.push(collect_command(method)?);
        }
        if is_periodic {
            periodic_methods.push(collect_periodic(method)?);
        }
    }

    Ok(CellDefinition {
        struct_ty,
        init_method,
        commands,
        event_handlers,
        periodic_methods,
    })
}

fn has_typed_arguments(method: &syn::ImplItemFn) -> bool {
    method
        .sig
        .inputs
        .iter()
        .any(|a| matches!(a, FnArg::Typed(_)))
}

fn has_receiver(method: &syn::ImplItemFn) -> bool {
    method
        .sig
        .inputs
        .iter()
        .any(|a| matches!(a, FnArg::Receiver(_)))
}

fn collect_init<'a>(method: &'a syn::ImplItemFn, struct_ty: &Type) -> syn::Result<InitMethod<'a>> {
    if has_receiver(method) {
        return Err(syn::Error::new_spanned(
            method,
            "#[init] must not take &self or &mut self",
        ));
    }

    if has_typed_arguments(method) {
        return Err(syn::Error::new_spanned(
            method,
            "#[init] must not take arguments",
        ));
    }

    if returns_result_of_self(&method.sig.output, struct_ty) {
        return Ok(InitMethod {
            method,
            returns_result: true,
        });
    }

    if returns_bare_self(&method.sig.output, struct_ty) {
        return Ok(InitMethod {
            method,
            returns_result: false,
        });
    }

    Err(syn::Error::new_spanned(
        &method.sig.output,
        "#[init] must return Self or Result<Self>",
    ))
}

fn collect_event_handler(method: &syn::ImplItemFn) -> syn::Result<EventHandlerMethod<'_>> {
    let receiver = match method.sig.inputs.first() {
        Some(FnArg::Receiver(r)) => Some(r.mutability.is_some()),
        _ => None,
    };
    let arg_buf = parse_arg_buf_attr(&method.attrs, KW_EVENT_HANDLER)?;
    Ok(EventHandlerMethod {
        method,
        receiver,
        arg_buf,
    })
}

fn collect_command(method: &syn::ImplItemFn) -> syn::Result<CommandMethod<'_>> {
    let receiver = match method.sig.inputs.first() {
        Some(FnArg::Receiver(r)) => Some(r.mutability.is_some()),
        _ => None,
    };
    let has_args = method
        .sig
        .inputs
        .iter()
        .any(|arg| matches!(arg, FnArg::Typed(_)));
    let (returns_result, return_ty) = parse_command_return_type(method);
    let arg_buf = parse_arg_buf_attr(&method.attrs, KW_COMMAND)?;
    Ok(CommandMethod {
        method,
        receiver,
        has_args,
        returns_result,
        return_ty,
        arg_buf,
    })
}

/// Parses an optional `arg_buf = <size>` argument from the `#[command(...)]`
/// or `#[event_handler(...)]` attribute. Returns `None` when the attribute
/// carries no parenthesized arguments.
fn parse_arg_buf_attr(attrs: &[Attribute], kw: &str) -> syn::Result<Option<usize>> {
    let attr = attrs
        .iter()
        .find(|a| a.path().is_ident(kw))
        .expect("caller checked the attribute is present");

    if matches!(attr.meta, Meta::Path(_)) {
        return Ok(None);
    }

    let metas: Punctuated<Meta, Comma> = attr.parse_args_with(Punctuated::parse_terminated)?;
    let mut arg_buf = None;

    for meta in &metas {
        let Meta::NameValue(nv) = meta else {
            return Err(syn::Error::new_spanned(
                meta,
                format!("expected `name = value` argument to #[{kw}(...)]"),
            ));
        };
        if !nv.path.is_ident("arg_buf") {
            return Err(syn::Error::new_spanned(
                &nv.path,
                format!("unknown #[{kw}(...)] argument; supported: `arg_buf`"),
            ));
        }
        arg_buf = Some(parse_buf_size(&nv.value)?);
    }

    Ok(arg_buf)
}

fn collect_periodic(method: &syn::ImplItemFn) -> syn::Result<PeriodicMethod<'_>> {
    if has_typed_arguments(method) {
        return Err(syn::Error::new_spanned(
            method,
            "#[periodic] methods must not take arguments",
        ));
    }
    if !matches!(method.sig.output, ReturnType::Default) {
        return Err(syn::Error::new_spanned(
            &method.sig.output,
            "#[periodic] methods must not return a value",
        ));
    }
    let (period, fixed_delay) = parse_period(method)?;
    let receiver = match method.sig.inputs.first() {
        Some(FnArg::Receiver(r)) => Some(r.mutability.is_some()),
        _ => None,
    };
    Ok(PeriodicMethod {
        method,
        receiver,
        period,
        fixed_delay,
    })
}

/// Parses the return type of a `#[command]` function.
/// Returns `(returns_result, return_ty)` where:
/// - `returns_result`: true if the user wrote `Result<T>` / `Result<()>`
/// - `return_ty`: the inner `T` (or the plain type), `None` for `()` / `Result<()>` / no return
fn parse_command_return_type(method: &syn::ImplItemFn) -> (bool, Option<&'_ Type>) {
    let ReturnType::Type(_, ty) = &method.sig.output else {
        return (false, None);
    };

    if let Some(inner) = extract_result_inner(ty) {
        let return_ty = if matches!(inner, Type::Tuple(t) if t.elems.is_empty()) {
            None
        } else {
            Some(inner)
        };
        (true, return_ty)
    } else {
        (false, Some(ty.as_ref()))
    }
}

/// If `ty` is `Result<T>` (with or without a path prefix), returns `Some(&T)`.
fn extract_result_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let last_segment = type_path.path.segments.last()?;
    if last_segment.ident != "Result" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &last_segment.arguments else {
        return None;
    };
    // Take the first type argument (Result<T> or Result<T, E>)
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

/// Returns true if the return type is bare `Self` or the concrete struct type.
fn returns_bare_self(output: &ReturnType, struct_ty: &Type) -> bool {
    let ReturnType::Type(_, ty) = output else {
        return false;
    };
    is_self_or_struct(ty, struct_ty)
}

/// Returns true if the return type is `Result<Self>` or `Result<StructName>`.
fn returns_result_of_self(output: &ReturnType, struct_ty: &Type) -> bool {
    let ReturnType::Type(_, ty) = output else {
        return false;
    };
    let Some(inner) = extract_result_inner(ty) else {
        return false;
    };
    is_self_or_struct(inner, struct_ty)
}

/// Returns true if `ty` is `Self` or the concrete struct type.
fn is_self_or_struct(ty: &Type, struct_ty: &Type) -> bool {
    let Type::Path(tp) = ty else {
        return false;
    };
    if tp.path.is_ident("Self") {
        return true;
    }
    if let Type::Path(struct_path) = struct_ty {
        return tp.path.is_ident(
            &struct_path
                .path
                .segments
                .last()
                .expect("struct type must have at least one segment")
                .ident,
        );
    }
    false
}

/// Parses the `#[periodic(...)]` attribute and returns the period plus
/// fixed-delay scheduling mode.
fn parse_period(method: &syn::ImplItemFn) -> syn::Result<(std::time::Duration, bool)> {
    let attr = method
        .attrs
        .iter()
        .find(|a| a.path().is_ident(KW_PERIODIC))
        .expect("caller checked is_periodic");

    let args = attr.parse_args_with(Punctuated::<Meta, Comma>::parse_terminated)?;
    let mut period = None;
    let mut fixed_delay = false;

    for meta in args {
        let Meta::NameValue(nv) = meta else {
            return Err(syn::Error::new_spanned(
                meta,
                "expected `every = \"...\"` or `wait_until_finished = <bool>`",
            ));
        };

        if nv.path.is_ident("every") {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(every_value),
                ..
            }) = &nv.value
            else {
                return Err(syn::Error::new_spanned(
                    &nv.value,
                    "expected a string literal for `every`",
                ));
            };

            period = Some(
                humantime::parse_duration(&every_value.value()).map_err(|e| {
                    syn::Error::new(every_value.span(), format!("invalid duration: {e}"))
                })?,
            );
        } else if nv.path.is_ident("wait_until_finished") {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Bool(wait_until_finished),
                ..
            }) = &nv.value
            else {
                return Err(syn::Error::new_spanned(
                    &nv.value,
                    "expected a boolean literal for `wait_until_finished`",
                ));
            };
            fixed_delay = wait_until_finished.value;
        } else {
            return Err(syn::Error::new_spanned(
                &nv.path,
                "expected `every = \"...\"` or `wait_until_finished = <bool>`",
            ));
        }
    }

    period
        .map(|period| (period, fixed_delay))
        .ok_or_else(|| syn::Error::new_spanned(attr, "expected `every = \"...\"`"))
}

/// Removes the cell macros from the methods so that we don't go all recursive.
#[must_use]
pub fn strip_cell_attrs(mut item_impl: ItemImpl) -> ItemImpl {
    for item in &mut item_impl.items {
        if let ImplItem::Fn(method) = item {
            method.attrs.retain(|a| {
                !a.path().is_ident(KW_INIT)
                    && !a.path().is_ident(KW_COMMAND)
                    && !a.path().is_ident(KW_EVENT_HANDLER)
                    && !a.path().is_ident(KW_PERIODIC)
            });
        }
    }
    item_impl
}
