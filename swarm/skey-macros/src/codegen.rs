use crate::ast;
use crate::ast::{KeyField, KeyVariant};
use darling::ast::{Fields, Style};
use proc_macro2::{Span, TokenStream};
use syn::spanned::Spanned;

pub fn expand(root: &TokenStream, input: ast::DeriveInput) -> TokenStream {
    let ident = input.ident;
    let span = ident.span();

    let generics = input.generics;

    let input_lifetime = generics
        .params
        .iter()
        .find(|p| matches!(p, syn::GenericParam::Lifetime(_)));

    let alloc;
    let input_lifetime = if let Some(lifetime) = input_lifetime {
        lifetime
    } else {
        alloc = syn::GenericParam::Lifetime(syn::parse_quote!('_));
        &alloc
    };

    let (encode, decode) = match input.data {
        ast::KeyData::Enum(data) => expand_enum(root, span, data),
        ast::KeyData::Struct(fields) => expand_struct(root, span, fields),
    };

    let (impl_gen, ty_gen, where_gen) = generics.split_for_impl();

    quote::quote_spanned! { span=>
        const _: () = {
            impl #impl_gen #root::StoreKey<#input_lifetime> for #ident #ty_gen #where_gen {
                fn encode_into(&self, encoder: &mut #root::Encoder<'_>) -> Result<(), #root::KeyError> {
                    #encode

                    Ok(())
                }

                fn decode_from(decoder: &mut #root::Decoder<#input_lifetime>) -> Result<Self, #root::KeyError> {
                    #decode
                }

            }
        };
    }
}

fn expand_enum(
    root: &TokenStream,
    span: Span,
    data: Vec<KeyVariant>,
) -> (TokenStream, TokenStream) {
    let mut encode_branches = quote::quote!();
    let mut decode_branches = quote::quote!();

    for (i, variant) in data.into_iter().enumerate() {
        let ident = &variant.ident;
        let span = variant.ident.span();

        let discriminator = if let Some(discriminant) = variant.discriminant {
            quote::quote!(#discriminant)
        } else {
            #[expect(
                clippy::expect_used,
                reason = "variant index from enumerate() cannot exceed u32::MAX"
            )]
            let discriminant = u32::try_from(i).expect("enum has more than u32::MAX variants");
            quote::quote!(#discriminant)
        };

        let style = variant.fields.style;

        let mut group = quote::quote!();

        let mut encode = quote::quote!();
        let mut decode = quote::quote!();

        for (i, field) in variant.fields.into_iter().enumerate() {
            let span = field
                .ident
                .as_ref()
                .map_or_else(|| field.ty.span(), proc_macro2::Ident::span);
            let ident = field
                .ident
                .unwrap_or_else(|| quote::format_ident!("_{}", i));

            group.extend(quote::quote_spanned! {span=>
                #ident,
            });

            if field.raw {
                encode.extend(quote::quote_spanned! {span=>
                    encoder.write_raw(#ident)?;
                });
            } else {
                encode.extend(quote::quote_spanned! {span=>
                    #root::StoreKey::encode_into(#ident, encoder)?;
                });
            }
            decode.extend(quote::quote_spanned! {span=>
                let #ident = #root::StoreKey::decode_from(decoder)?;
            });
        }

        let group = match style {
            Style::Struct => {
                quote::quote_spanned! {span=>
                    {
                        #group
                    }
                }
            }
            Style::Tuple => {
                quote::quote_spanned! {span=>
                    (
                        #group
                    )
                }
            }
            Style::Unit => {
                quote::quote!()
            }
        };

        encode_branches.extend(quote::quote_spanned! {span=>
            Self:: #ident #group => {
                #[allow(clippy::unnecessary_cast)]
                #root::StoreKey::encode_into(&(#discriminator as u32), encoder)?;
                #encode
            }
        });
        decode_branches.extend(quote::quote_spanned! {span=>
            #discriminator => {
                #decode

                Ok(Self:: #ident #group)
            }
        });
    }

    let encode = quote::quote_spanned! {span=>
        match self {
            #encode_branches
        }
    };

    let decode = quote::quote_spanned! {span=>
        match <u32 as #root::StoreKey>::decode_from(decoder)? {
            #decode_branches
            id => Err(skey::KeyError::msg(format!("unknown id: {}", id))),
        }
    };

    (encode, decode)
}

fn expand_struct(
    root: &TokenStream,
    span: Span,
    fields: Fields<KeyField>,
) -> (TokenStream, TokenStream) {
    let mut destructure = quote::quote! {};
    let mut read = quote::quote! {};
    let mut write = quote::quote! {};

    for field in fields {
        #[expect(
            clippy::expect_used,
            reason = "darling `supports(struct_named)` guarantees struct fields are named"
        )]
        let field_ident = field
            .ident
            .clone()
            .expect("named struct field always has an ident");
        let span = field_ident.span();

        destructure.extend(quote::quote_spanned! {span=>
            #field_ident,
        });

        if field.raw {
            write.extend(quote::quote_spanned! {span=>
                encoder.write_raw(#field_ident)?;
            });
        } else {
            write.extend(quote::quote_spanned! {span=>
                #root::StoreKey::encode_into(#field_ident, encoder)?;
            });
        }
        read.extend(quote::quote_spanned! {span=>
            #field_ident: #root::StoreKey::decode_from(decoder)?,
        });
    }

    let encode = quote::quote_spanned! {span=>
        let Self {
            #destructure
        } = self;

        #write
    };
    let decode = quote::quote_spanned! {span=>
        Ok(Self {
            #read
        })
    };

    (encode, decode)
}
