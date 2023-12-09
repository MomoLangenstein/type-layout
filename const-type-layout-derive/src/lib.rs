#![deny(clippy::pedantic)]
#![feature(iter_intersperse)]

extern crate proc_macro;

#[macro_use]
extern crate proc_macro_error;

use proc_macro::TokenStream;

use proc_macro2::Literal;
use quote::{quote, quote_spanned};
use syn::{parse_macro_input, spanned::Spanned};

#[proc_macro_error]
#[proc_macro_derive(TypeLayout, attributes(layout))]
pub fn derive_type_layout(input: TokenStream) -> TokenStream {
    // Parse the input tokens into a syntax tree
    let input = parse_macro_input!(input as syn::DeriveInput);

    // Used in the quasi-quotation below as `#ty_name`.
    let ty_name = input.ident;
    let ty_generics = input.generics.split_for_impl().1;

    let mut type_params = input
        .generics
        .type_params()
        .map(|param| &param.ident)
        .collect::<Vec<_>>();

    let Attributes {
        reprs,
        extra_bounds,
        crate_path,
    } = parse_attributes(&input.attrs, &mut type_params);

    let layout = layout_of_type(&crate_path, &ty_name, &ty_generics, &input.data, &reprs);

    let inner_types = extract_inner_types(&input.data);

    let discriminant_ty = if let syn::Data::Enum(_) = input.data {
        Some(quote! { <Self as #crate_path::ExtractDiscriminant>::Ty, })
    } else {
        None
    };

    let Generics {
        type_layout_input_generics,
        type_set_input_generics,
    } = generate_generics(&crate_path, &input.generics, &extra_bounds, &type_params);
    let (type_layout_impl_generics, type_layout_ty_generics, type_layout_where_clause) =
        type_layout_input_generics.split_for_impl();
    let (type_set_impl_generics, type_set_ty_generics, type_set_where_clause) =
        type_set_input_generics.split_for_impl();

    quote! {
        unsafe impl #type_layout_impl_generics #crate_path::TypeLayout for
            #ty_name #type_layout_ty_generics #type_layout_where_clause
        {
            const TYPE_LAYOUT: #crate_path::TypeLayoutInfo<'static> = {
                #crate_path::TypeLayoutInfo {
                    name: ::core::any::type_name::<Self>(),
                    size: ::core::mem::size_of::<Self>(),
                    alignment: ::core::mem::align_of::<Self>(),
                    structure: #layout,
                }
            };
        }

        unsafe impl #type_set_impl_generics #crate_path::typeset::ComputeTypeSet for
            #ty_name #type_set_ty_generics #type_set_where_clause
        {
            type Output<__TypeSetRest: #crate_path::typeset::ExpandTypeSet> =
                #crate_path::typeset::Set<Self, #crate_path::typeset::tset![
                    #(#inner_types,)* #discriminant_ty .. @ __TypeSetRest
                ]>;
        }
    }
    .into()
}

struct Attributes {
    reprs: String,
    extra_bounds: Vec<syn::WherePredicate>,
    crate_path: syn::Path,
}

#[allow(clippy::too_many_lines)]
fn parse_attributes(attrs: &[syn::Attribute], type_params: &mut Vec<&syn::Ident>) -> Attributes {
    // Could parse based on https://github.com/rust-lang/rust/blob/d13e8dd41d44a73664943169d5b7fe39b22c449f/compiler/rustc_attr/src/builtin.rs#L772-L781 instead
    let mut reprs = Vec::new();

    let mut extra_bounds: Vec<syn::WherePredicate> = Vec::new();

    let mut crate_path = None;

    for attr in attrs {
        if attr.path.is_ident("repr") {
            if let Ok(syn::Meta::List(syn::MetaList { nested, .. })) = attr.parse_meta() {
                for meta in nested {
                    reprs.push(match meta {
                        syn::NestedMeta::Lit(lit) => lit_to_string(&lit),
                        syn::NestedMeta::Meta(meta) => meta_to_string(&meta),
                    });
                }
            } else {
                emit_warning!(
                    attr.span(),
                    "[const-type-layout]: #[repr] attribute is not in meta list format."
                );
            }
        } else if attr.path.is_ident("layout") {
            if let Ok(syn::Meta::List(list)) = attr.parse_meta() {
                for meta in &list.nested {
                    if let syn::NestedMeta::Meta(syn::Meta::NameValue(syn::MetaNameValue {
                        path,
                        lit: syn::Lit::Str(s),
                        ..
                    })) = &meta
                    {
                        if path.is_ident("free") {
                            match syn::parse_str::<syn::Ident>(&s.value()) {
                                Ok(param) => {
                                    if let Some(i) = type_params.iter().position(|ty| **ty == param)
                                    {
                                        type_params.swap_remove(i);
                                    } else {
                                        emit_error!(
                                            s.span(),
                                            "[const-type-layout]: Invalid #[layout(free)] \
                                             attribute: \"{}\" is either not a type parameter or \
                                             has already been freed (duplicate attribute).",
                                            param,
                                        );
                                    }
                                },
                                Err(err) => emit_error!(
                                    s.span(),
                                    "[const-type-layout]: Invalid #[layout(free = \"<type>\")] \
                                     attribute: {}.",
                                    err
                                ),
                            }
                        } else if path.is_ident("bound") {
                            match syn::parse_str(&s.value()) {
                                Ok(bound) => extra_bounds.push(bound),
                                Err(err) => emit_error!(
                                    s.span(),
                                    "[const-type-layout]: Invalid #[layout(bound = \
                                     \"<where-predicate>\")] attribute: {}.",
                                    err
                                ),
                            }
                        } else if path.is_ident("crate") {
                            match syn::parse_str::<syn::Path>(&s.value()) {
                                Ok(new_crate_path) => {
                                    if crate_path.is_none() {
                                        crate_path = Some(
                                            syn::parse_quote_spanned! { s.span() => #new_crate_path },
                                        );
                                    } else {
                                        emit_error!(
                                            s.span(),
                                            "[const-type-layout]: Duplicate #[layout(crate)] \
                                             attribute: the crate path for `const-type-layout` \
                                             can only be set once per `derive`.",
                                        );
                                    }
                                },
                                Err(err) => emit_error!(
                                    s.span(),
                                    "[const-type-layout]: Invalid #[layout(crate = \
                                     \"<crate-path>\")] attribute: {}.",
                                    err
                                ),
                            }
                        } else {
                            emit_error!(
                                path.span(),
                                "[const-type-layout]: Unknown attribute, use `bound`, `crate`, \
                                 `free`, or `ground`."
                            );
                        }
                    } else {
                        emit_error!(
                            meta.span(),
                            "[const-type-layout]: Expected #[layout(attr = \"value\")] syntax."
                        );
                    }
                }
            } else {
                emit_error!(
                    attr.span(),
                    "[const-type-layout]: Expected #[layout(attr = \"value\")] syntax."
                );
            }
        }
    }

    proc_macro_error::abort_if_dirty();

    reprs.sort();
    reprs.dedup();

    let reprs = reprs
        .into_iter()
        .intersperse(String::from(","))
        .collect::<String>();

    Attributes {
        reprs,
        extra_bounds,
        crate_path: crate_path.unwrap_or_else(|| syn::parse_quote!(::const_type_layout)),
    }
}

fn meta_to_string(meta: &syn::Meta) -> String {
    match meta {
        syn::Meta::List(syn::MetaList { path, nested, .. }) => {
            let mut list = nested
                .iter()
                .map(|meta| match meta {
                    syn::NestedMeta::Lit(lit) => lit_to_string(lit),
                    syn::NestedMeta::Meta(meta) => meta_to_string(meta),
                })
                .collect::<Vec<_>>();
            list.sort();
            list.dedup();

            format!(
                "{}({})",
                quote!(#path),
                list.into_iter()
                    .intersperse(String::from(","))
                    .collect::<String>()
            )
        },
        syn::Meta::NameValue(syn::MetaNameValue { path, lit, .. }) => {
            format!("{}={}", quote!(#path), lit_to_string(lit))
        },
        syn::Meta::Path(path) => quote!(#path).to_string(),
    }
}

fn lit_to_string(lit: &syn::Lit) -> String {
    quote!(#lit).to_string().escape_default().to_string()
}

fn extract_inner_types(data: &syn::Data) -> Vec<&syn::Type> {
    let mut inner_types = Vec::new();

    match data {
        syn::Data::Struct(syn::DataStruct { fields, .. }) => {
            for field in fields {
                inner_types.push(&field.ty);
            }
        },
        syn::Data::Union(syn::DataUnion {
            fields: syn::FieldsNamed { named: fields, .. },
            ..
        }) => {
            for field in fields {
                inner_types.push(&field.ty);
            }
        },
        syn::Data::Enum(syn::DataEnum { variants, .. }) => {
            for variant in variants {
                for field in &variant.fields {
                    inner_types.push(&field.ty);
                }
            }
        },
    }

    inner_types
}

struct Generics {
    type_layout_input_generics: syn::Generics,
    type_set_input_generics: syn::Generics,
}

fn generate_generics(
    crate_path: &syn::Path,
    generics: &syn::Generics,
    extra_bounds: &[syn::WherePredicate],
    type_params: &[&syn::Ident],
) -> Generics {
    let mut type_layout_input_generics = generics.clone();
    let mut type_set_input_generics = generics.clone();

    for ty in type_params {
        type_layout_input_generics
            .make_where_clause()
            .predicates
            .push(syn::parse_quote! {
                #ty: #crate_path::TypeLayout
            });

        type_set_input_generics
            .make_where_clause()
            .predicates
            .push(syn::parse_quote! {
                #ty: #crate_path::typeset::ComputeTypeSet
            });
    }

    for bound in extra_bounds {
        type_layout_input_generics
            .make_where_clause()
            .predicates
            .push(bound.clone());

        type_set_input_generics
            .make_where_clause()
            .predicates
            .push(bound.clone());
    }

    Generics {
        type_layout_input_generics,
        type_set_input_generics,
    }
}

fn layout_of_type(
    crate_path: &syn::Path,
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    data: &syn::Data,
    reprs: &str,
) -> proc_macro2::TokenStream {
    match data {
        syn::Data::Struct(data) => {
            let fields = quote_structlike_fields(crate_path, ty_name, ty_generics, &data.fields);

            quote! {
                #crate_path::TypeStructure::Struct { repr: #reprs, fields: &[#(#fields),*] }
            }
        },
        syn::Data::Enum(r#enum) => {
            let variants = quote_enum_variants(crate_path, ty_name, ty_generics, r#enum);

            quote! {
                #crate_path::TypeStructure::Enum { repr: #reprs, variants: &[#(#variants),*] }
            }
        },
        syn::Data::Union(union) => {
            let fields = quote_structlike_fields(
                crate_path,
                ty_name,
                ty_generics,
                &syn::Fields::Named(union.fields.clone()),
            );

            quote! {
                #crate_path::TypeStructure::Union { repr: #reprs, fields: &[#(#fields),*] }
            }
        },
    }
}

fn quote_structlike_fields(
    crate_path: &syn::Path,
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    fields: &syn::Fields,
) -> Vec<proc_macro2::TokenStream> {
    match fields {
        syn::Fields::Named(fields) => fields
            .named
            .iter()
            .map(|field| {
                let field_name = field.ident.as_ref().unwrap();
                let field_name_str = Literal::string(&field_name.to_string());
                let field_ty = &field.ty;
                let field_offset =
                    quote_structlike_field_offset(crate_path, ty_name, ty_generics, &field_name);

                quote_spanned! { field.span() =>
                    #crate_path::Field {
                        name: #field_name_str,
                        offset: { #field_offset },
                        ty: ::core::any::type_name::<#field_ty>(),
                    }
                }
            })
            .collect(),
        syn::Fields::Unnamed(fields) => fields
            .unnamed
            .iter()
            .enumerate()
            .map(|(field_index, field)| {
                let field_name = syn::Index::from(field_index);
                let field_name_str = Literal::string(&field_index.to_string());
                let field_ty = &field.ty;
                let field_offset =
                    quote_structlike_field_offset(crate_path, ty_name, ty_generics, &field_name);

                quote_spanned! { field.span() =>
                    #crate_path::Field {
                        name: #field_name_str,
                        offset: { #field_offset },
                        ty: ::core::any::type_name::<#field_ty>(),
                    }
                }
            })
            .collect(),
        syn::Fields::Unit => vec![],
    }
}

fn quote_structlike_field_offset(
    crate_path: &syn::Path,
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    field_name: &impl quote::ToTokens,
) -> proc_macro2::TokenStream {
    quote! {
        // TODO: check for uninhabited
        #crate_path::MaybeUninhabited::Inhabited(::core::mem::offset_of!(#ty_name #ty_generics, #field_name))
    }
}

fn quote_enum_variants(
    crate_path: &syn::Path,
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    r#enum: &syn::DataEnum,
) -> Vec<proc_macro2::TokenStream> {
    let mut last_discriminant = syn::Expr::Lit(syn::ExprLit {
        attrs: vec![],
        lit: syn::Lit::Int(syn::LitInt::from(proc_macro2::Literal::usize_unsuffixed(0))),
    });
    let mut last_discriminant_offset = 0;

    r#enum
        .variants
        .iter()
        .map(|variant| {
            let variant_name = &variant.ident;
            let variant_name_str = Literal::string(&variant_name.to_string());

            let fields = quote_variant_fields(
                crate_path,
                ty_name,
                ty_generics,
                variant_name,
                &variant.fields,
            );

            let discriminant = match variant.discriminant.as_ref() {
                None => {
                    let discriminant = syn::Expr::Binary(syn::ExprBinary {
                        attrs: vec![],
                        left: Box::new(last_discriminant.clone()),
                        op: syn::parse_quote!(+),
                        right: Box::new(syn::Expr::Lit(syn::ExprLit {
                            attrs: vec![],
                            lit: syn::Lit::Int(syn::LitInt::from(
                                proc_macro2::Literal::usize_unsuffixed(last_discriminant_offset),
                            )),
                        })),
                    });
                    last_discriminant_offset += 1;
                    discriminant
                },
                Some((_, discriminant)) => {
                    last_discriminant = discriminant.clone();
                    last_discriminant_offset = 0;
                    discriminant.clone()
                },
            };

            let discriminant = quote! {
                #crate_path::MaybeUninhabited::Inhabited(
                    #crate_path::Discriminant::new::<Self>(#discriminant)
                )
            };

            quote! {
                #crate_path::Variant {
                    name: #variant_name_str,
                    discriminant: #discriminant,
                    fields: &[#(#fields),*],
                }
            }
        })
        .collect::<Vec<_>>()
}

fn quote_variant_fields(
    crate_path: &syn::Path,
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    variant_name: &syn::Ident,
    variant_fields: &syn::Fields,
) -> Vec<proc_macro2::TokenStream> {
    match variant_fields {
        syn::Fields::Named(syn::FieldsNamed { named: fields, .. }) => fields
            .iter()
            .map(|field| {
                let field_name_str = Literal::string(&field.ident.as_ref().unwrap().to_string());
                let field_name = &field.ident;
                let field_ty = &field.ty;

                let offset = quote_structlike_variant_field_offset(
                    crate_path,
                    ty_name,
                    ty_generics,
                    variant_name,
                    field_name,
                );

                quote_spanned! { field.span() =>
                    #crate_path::Field {
                        name: #field_name_str,
                        offset: #offset,
                        ty: ::core::any::type_name::<#field_ty>(),
                    }
                }
            })
            .collect(),
        syn::Fields::Unnamed(syn::FieldsUnnamed {
            unnamed: fields, ..
        }) => fields
            .iter()
            .enumerate()
            .map(|(field_index, field)| {
                let field_name_str = Literal::string(&field_index.to_string());
                let field_index = syn::Index::from(field_index);
                let field_ty = &field.ty;

                let offset = quote_structlike_variant_field_offset(
                    crate_path,
                    ty_name,
                    ty_generics,
                    variant_name,
                    &field_index,
                );

                quote_spanned! { field.span() =>
                    #crate_path::Field {
                        name: #field_name_str,
                        offset: #offset,
                        ty: ::core::any::type_name::<#field_ty>(),
                    }
                }
            })
            .collect(),
        syn::Fields::Unit => vec![],
    }
}

fn quote_structlike_variant_field_offset(
    crate_path: &syn::Path,
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    variant_name: &syn::Ident,
    field_name: &impl quote::ToTokens,
) -> proc_macro2::TokenStream {
    quote! {
        // TODO: check for uninhabited
        #crate_path::MaybeUninhabited::Inhabited(::core::mem::offset_of!(#ty_name #ty_generics, #variant_name.#field_name))
    }
}
