use std::collections::BTreeMap;

use proc_macro2::TokenStream;
use quote::format_ident;

use super::model::{AliasModel, KeyDef, KeyModel, Segment, SegmentKind, TreeDsl, TreeItem};
use super::util::{
    alias_terminal_ty, child_keys, child_target_state_index, collect_fields, decode_steps,
    encode_steps, field_slice_matches, fields_prefix, kind_to_arg_ty, kind_to_assign_expr,
    kind_to_storage_ty, resolve_aliases, state_ident,
};

/// Converts the parsed DSL model into generated key/builder Rust code.
/// It resolves aliases, validates prefix relationships, and emits the fluent API surface.
#[expect(
    clippy::too_many_lines,
    reason = "TODO: split expand into per-stage helpers"
)]
pub fn expand(input: TreeDsl) -> syn::Result<TokenStream> {
    let mut aliases =
        BTreeMap::<String, (proc_macro2::Ident, Vec<super::model::SegmentSpec>)>::new();
    let mut types = BTreeMap::<String, (proc_macro2::Ident, syn::Type, bool)>::new();
    let mut keys = Vec::<KeyDef>::new();

    for item in input.items {
        match item {
            TreeItem::Alias(alias) => {
                let name = alias.name.to_string();
                let old = aliases.insert(name, (alias.name.clone(), alias.segments));
                if old.is_some() {
                    return Err(syn::Error::new(alias.name.span(), "duplicate alias name"));
                }
            }
            TreeItem::Type(ty) => {
                let name = ty.name.to_string();
                let old = types.insert(name, (ty.name.clone(), ty.ty, ty.no_copy));
                if old.is_some() {
                    return Err(syn::Error::new(ty.name.span(), "duplicate type alias name"));
                }
            }
            TreeItem::Key(key) => keys.push(key),
        }
    }

    let no_copy_types = types
        .iter()
        .filter(|(_, (_, _, no_copy))| *no_copy)
        .map(|(_, (name, _, _))| name.to_string())
        .collect::<std::collections::BTreeSet<_>>();

    // We want to collect the aliases so we can add the helper functions to builders that have those types.
    // ie, if we wrote:
    // ```
    // alias scope("@", name: str, "*", other: str);
    // key my_key(scope, "sd", thing: str);
    // ```
    // Then it's possible to do something like:
    // ```
    // Key::my_key().scope(scope).thing("blah")
    // vs
    // Key::my_key().name("name").other("").thing("blah")
    // ```

    let mut alias_models = Vec::<AliasModel>::new();
    for (alias_name, segments) in aliases.values() {
        let segments = resolve_aliases(segments, &aliases, &mut Vec::new())?;
        alias_models.push(AliasModel {
            name: alias_name.clone(),
            fields: collect_fields(&segments)?,
        });
    }

    // Resolve all the aliases in the keys.
    let mut models = Vec::new();
    for key in keys {
        let mut segments = resolve_aliases(&key.segments, &aliases, &mut Vec::new())?;
        for segment in &mut segments {
            if let Segment::Field {
                kind: SegmentKind::Type { ty, repr },
                ..
            } = segment
                && let Some((_, alias_ty, _)) = types.get(repr)
            {
                **ty = alias_ty.clone();
            }
        }
        let fields = collect_fields(&segments)?;

        let has_borrowed_fields = fields
            .iter()
            .any(|(_, kind)| matches!(kind, SegmentKind::Str | SegmentKind::Bytes));
        let no_copy = fields.iter().any(|(_, kind)| {
            matches!(
                kind,
                SegmentKind::Type { repr, .. } if no_copy_types.contains(repr)
            )
        });
        models.push(KeyModel {
            name: key.name,
            docs: key.docs,
            no_copy,
            segments,
            fields,
            has_borrowed_fields,
        });
    }
    let keys_by_name = models
        .iter()
        .map(|model| (model.name.to_string(), model))
        .collect::<BTreeMap<_, _>>();

    let mut key_methods = Vec::new();
    let mut builder_items = Vec::new();

    for model in &models {
        let model_span = model.name.span();
        let start_ident = state_ident(&model.name, 0);
        let start_name = &model.name;
        let key_docs = &model.docs;

        if model.has_borrowed_fields {
            key_methods.push(quote::quote_spanned! { model_span=>
                #( #key_docs )*
                pub fn #start_name<'a>() -> #start_ident<'a> {
                    #start_ident {
                        _marker: ::core::marker::PhantomData,
                    }
                }
            });
        } else {
            key_methods.push(quote::quote_spanned! { model_span=>
                #( #key_docs )*
                pub fn #start_name() -> #start_ident {
                    #start_ident {}
                }
            });
        }

        let field_count = model.fields.len();
        if field_count > 0 {
            let term_ident = state_ident(&model.name, field_count);
            let full_name = format_ident!("new_{}", start_name);

            let full_args: Vec<TokenStream> = model
                .fields
                .iter()
                .map(|(name, kind)| {
                    let arg_ty = kind_to_arg_ty(kind, model.has_borrowed_fields);
                    quote::quote_spanned!(name.span()=> #name: #arg_ty)
                })
                .collect();
            let full_inits: Vec<TokenStream> = model
                .fields
                .iter()
                .map(|(name, kind)| {
                    let assign_expr = kind_to_assign_expr(kind, quote::quote!(#name));
                    quote::quote_spanned!(name.span()=> #name: #assign_expr)
                })
                .collect();

            let ts = if model.has_borrowed_fields {
                quote::quote_spanned! { model_span=>
                    #[allow(clippy::too_many_arguments)]
                    pub fn #full_name<'a>( #( #full_args ),* ) -> #term_ident<'a> {
                        #term_ident {
                            #( #full_inits, )*
                            _marker: ::core::marker::PhantomData,
                        }
                    }
                }
            } else {
                quote::quote_spanned! { model_span=>
                    #[allow(clippy::too_many_arguments)]
                    pub fn #full_name( #( #full_args ),* ) -> #term_ident {
                        #term_ident {
                            #( #full_inits, )*
                        }
                    }
                }
            };
            key_methods.push(ts);
        }

        // Each iteration is creates a unique Builder containing only the subset of fields up to that point.
        // This means we can do this like `Key::scope().namespace("doggo").database("brava")`;
        for i in 0..=field_count {
            let this_ident = state_ident(&model.name, i);
            let this_fields = fields_prefix(&model.fields, i);
            let this_defs: Vec<TokenStream> = this_fields
                .iter()
                .map(|(name, kind)| {
                    let ty = kind_to_storage_ty(kind, model.has_borrowed_fields);
                    quote::quote_spanned!(name.span()=> pub #name: #ty)
                })
                .collect();

            let mut methods = Vec::new();

            // Add the next step in the builder
            if i < field_count {
                let (next_name, next_kind) = &model.fields[i];
                let next_ident = state_ident(&model.name, i + 1);
                let arg_ty = kind_to_arg_ty(next_kind, model.has_borrowed_fields);
                let assign_expr =
                    kind_to_assign_expr(next_kind, quote::quote_spanned!(next_name.span()=> value));

                let move_prev: Vec<TokenStream> = this_fields
                    .iter()
                    .map(|(name, _)| quote::quote_spanned!(name.span()=> #name: self.#name))
                    .collect();

                if model.has_borrowed_fields {
                    methods.push(quote::quote_spanned! { next_name.span()=>
                        pub fn #next_name(self, value: #arg_ty) -> #next_ident<'a> {
                            #next_ident {
                                #( #move_prev, )*
                                #next_name: #assign_expr,
                                _marker: ::core::marker::PhantomData,
                            }
                        }
                    });
                } else {
                    methods.push(quote::quote_spanned! { next_name.span()=>
                        pub fn #next_name(self, value: #arg_ty) -> #next_ident {
                            #next_ident {
                                #( #move_prev, )*
                                #next_name: #assign_expr,
                            }
                        }
                    });
                }
            }

            for alias in &alias_models {
                let alias_len = alias.fields.len();
                if alias_len == 0 || i + alias_len > field_count {
                    continue;
                }
                if !field_slice_matches(&model.fields, i, &alias.fields) {
                    continue;
                }
                let Some(alias_key) = keys_by_name.get(&alias.name.to_string()) else {
                    continue;
                };
                let alias_arg_ty = alias_terminal_ty(alias_key);
                let alias_name = &alias.name;
                let next_ident = state_ident(&model.name, i + alias_len);
                let move_prev: Vec<TokenStream> = this_fields
                    .iter()
                    .map(|(name, _)| quote::quote_spanned!(name.span()=> #name: self.#name))
                    .collect();
                let move_alias: Vec<TokenStream> = alias
                    .fields
                    .iter()
                    .map(|(name, _)| quote::quote_spanned!(name.span()=> #name: value.#name))
                    .collect();

                if model.has_borrowed_fields {
                    methods.push(quote::quote_spanned! { alias_name.span()=>
                        pub fn #alias_name(self, value: #alias_arg_ty) -> #next_ident<'a> {
                            #next_ident {
                                #( #move_prev, )*
                                #( #move_alias, )*
                                _marker: ::core::marker::PhantomData,
                            }
                        }
                    });
                } else {
                    methods.push(quote::quote_spanned! { alias_name.span()=>
                        pub fn #alias_name(self, value: #alias_arg_ty) -> #next_ident {
                            #next_ident {
                                #( #move_prev, )*
                                #( #move_alias, )*
                            }
                        }
                    });
                }
            }

            // We're generating the last builder, so at this point, we can add key transitions.
            // ie, `scope -> table` vs having to create a `table` key from the start.
            if i == field_count {
                for child in child_keys(model, &models)? {
                    let child_name = &child.name;
                    let child_target = child_target_state_index(model, child)?;

                    let child_fields = fields_prefix(&child.fields, child_target);
                    let move_shared: Vec<TokenStream> = child_fields
                        .iter()
                        .map(|(name, _)| quote::quote_spanned!(name.span()=> #name: self.#name))
                        .collect();

                    let child_ident = state_ident(&child.name, child_target);

                    let ts = if child_target < child.fields.len() {
                        let (arg_name, arg_kind) = &child.fields[child_target];
                        let arg_ty = kind_to_arg_ty(arg_kind, child.has_borrowed_fields);
                        let assign_expr = kind_to_assign_expr(arg_kind, quote::quote!(#arg_name));
                        let child_next_ident = state_ident(&child.name, child_target + 1);
                        let child_only_name = format_ident!("only_{}", child_name);

                        if model.has_borrowed_fields {
                            quote::quote_spanned! { child_name.span()=>
                                pub fn #child_only_name(self) -> #child_ident<'a> {
                                    #child_ident {
                                        #( #move_shared, )*
                                        _marker: ::core::marker::PhantomData,
                                    }
                                }

                                pub fn #child_name(self, #arg_name: #arg_ty) -> #child_next_ident<'a> {
                                    #child_next_ident {
                                        #( #move_shared, )*
                                        #arg_name: #assign_expr,
                                        _marker: ::core::marker::PhantomData,
                                    }
                                }
                            }
                        } else {
                            quote::quote_spanned! { child_name.span()=>
                                pub fn #child_only_name(self) -> #child_ident {
                                    #child_ident {
                                        #( #move_shared, )*
                                    }
                                }

                                pub fn #child_name(self, #arg_name: #arg_ty) -> #child_next_ident {
                                    #child_next_ident {
                                        #( #move_shared, )*
                                        #arg_name: #assign_expr,
                                    }
                                }
                            }
                        }
                    } else if model.has_borrowed_fields {
                        quote::quote_spanned! { child_name.span()=>
                            pub fn #child_name(self) -> #child_ident<'a> {
                                #child_ident {
                                    #( #move_shared, )*
                                    _marker: ::core::marker::PhantomData,
                                }
                            }
                        }
                    } else {
                        quote::quote_spanned! { child_name.span()=>
                            pub fn #child_name(self) -> #child_ident {
                                #child_ident {
                                    #( #move_shared, )*
                                }
                            }
                        }
                    };

                    methods.push(ts);
                }
            }

            let derives = if model.no_copy {
                quote::quote_spanned!(model_span=> #[derive(Debug, Clone)])
            } else {
                quote::quote_spanned!(model_span=> #[derive(Debug, Clone, Copy)])
            };

            let encode_steps = encode_steps(&model.segments, i);
            let decode_steps = decode_steps(&model.segments, i, model.has_borrowed_fields);
            let decode_inits: Vec<TokenStream> = this_fields
                .iter()
                .map(|(name, _)| quote::quote!(#name))
                .collect();

            let ts = if model.has_borrowed_fields {
                quote::quote_spanned! { model_span=>
                    #derives
                    pub struct #this_ident<'a> {
                        #( #this_defs, )*
                        _marker: ::core::marker::PhantomData<&'a ()>,
                    }

                    #[allow(clippy::elidable_lifetime_names)]
                    impl<'a> #this_ident<'a> {
                        #( #methods )*
                    }

                    #[allow(clippy::elidable_lifetime_names)]
                    impl<'a> ::skey::StoreKey<'a> for #this_ident<'a> {
                        fn encode_into(&self, encoder: &mut ::skey::Encoder<'_>) -> Result<(), ::skey::KeyError> {
                            #( #encode_steps )*
                            Ok(())
                        }

                        fn decode_from(decoder: &mut ::skey::Decoder<'a>) -> Result<Self, ::skey::KeyError> {
                            #( #decode_steps )*
                            Ok(Self {
                                #( #decode_inits, )*
                                _marker: ::core::marker::PhantomData,
                            })
                        }
                    }
                }
            } else {
                quote::quote_spanned! { model_span=>
                    #derives
                    pub struct #this_ident {
                        #( #this_defs, )*
                    }

                    impl #this_ident {
                        #( #methods )*
                    }

                    #[allow(clippy::elidable_lifetime_names)]
                    impl<'a> ::skey::StoreKey<'a> for #this_ident {
                        fn encode_into(&self, encoder: &mut ::skey::Encoder<'_>) -> Result<(), ::skey::KeyError> {
                            #( #encode_steps )*
                            Ok(())
                        }

                        fn decode_from(decoder: &mut ::skey::Decoder<'a>) -> Result<Self, ::skey::KeyError> {
                            #( #decode_steps )*
                            Ok(Self {
                                #( #decode_inits, )*
                            })
                        }
                    }
                }
            };
            builder_items.push(ts);
        }
    }

    Ok(quote::quote! {
        pub struct Key;

        #[allow(clippy::elidable_lifetime_names)]
        impl Key {
            #( #key_methods )*
        }

        #( #builder_items )*
    })
}
