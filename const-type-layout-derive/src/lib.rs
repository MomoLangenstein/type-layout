#![deny(clippy::pedantic)]
#![feature(iter_intersperse)]
#![feature(let_else)]

extern crate proc_macro;

#[macro_use]
extern crate proc_macro_error;

use proc_macro::TokenStream;

use proc_macro2::Literal;
use quote::{quote, quote_spanned};
use syn::{parse_macro_input, spanned::Spanned};

// TODO:
// - wait for `const_heap` feature to be implemented for a workaround the graph
//   size limitation: https://github.com/rust-lang/rust/issues/79597

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
        ground,
    } = parse_attributes(&input.attrs, &mut type_params, &input.data);

    let layout = layout_of_type(&ty_name, &ty_generics, &input.data, &reprs);

    let uninit = uninit_for_type(&ty_name, &input.data, ground.as_ref());

    let inner_types = extract_inner_types(&input.data);

    let Generics {
        type_layout_input_generics,
        type_graph_input_generics,
    } = generate_generics(
        &ty_name,
        &ty_generics,
        &input.generics,
        matches!(input.data, syn::Data::Enum(_)),
        &extra_bounds,
        &type_params,
    );
    let (type_layout_impl_generics, type_layout_ty_generics, type_layout_where_clause) =
        type_layout_input_generics.split_for_impl();
    let (type_graph_impl_generics, type_graph_ty_generics, type_graph_where_clause) =
        type_graph_input_generics.split_for_impl();

    quote! {
        unsafe impl #type_layout_impl_generics const ::const_type_layout::TypeLayout for
            #ty_name #type_layout_ty_generics #type_layout_where_clause
        {
            const TYPE_LAYOUT: ::const_type_layout::TypeLayoutInfo<'static> = {
                ::const_type_layout::TypeLayoutInfo {
                    name: ::core::any::type_name::<Self>(),
                    size: ::core::mem::size_of::<Self>(),
                    alignment: ::core::mem::align_of::<Self>(),
                    structure: #layout,
                }
            };

            #[allow(unreachable_code)]
            unsafe fn uninit() -> ::core::mem::ManuallyDrop<Self> {
                ::core::mem::ManuallyDrop::new(
                    #uninit
                )
            }
        }

        unsafe impl #type_graph_impl_generics const ::const_type_layout::TypeGraph for
            #ty_name #type_graph_ty_generics #type_graph_where_clause
        {
            fn populate_graph(graph: &mut ::const_type_layout::TypeLayoutGraph<'static>) {
                if graph.insert(&<Self as ::const_type_layout::TypeLayout>::TYPE_LAYOUT) {
                    #(<#inner_types as ::const_type_layout::TypeGraph>::populate_graph(graph);)*
                }
            }
        }
    }
    .into()
}

struct Attributes {
    reprs: String,
    extra_bounds: Vec<syn::WherePredicate>,
    ground: Option<syn::Ident>,
}

#[allow(clippy::too_many_lines)]
fn parse_attributes(
    attrs: &[syn::Attribute],
    type_params: &mut Vec<&syn::Ident>,
    data: &syn::Data,
) -> Attributes {
    // Could parse based on https://github.com/rust-lang/rust/blob/d13e8dd41d44a73664943169d5b7fe39b22c449f/compiler/rustc_attr/src/builtin.rs#L772-L781 instead
    let mut reprs = Vec::new();

    let mut extra_bounds: Vec<syn::WherePredicate> = Vec::new();

    let mut ground = None;

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
                        } else if path.is_ident("ground") {
                            match syn::parse_str(&s.value()) {
                                Ok(g) => match data {
                                    syn::Data::Struct(_) => emit_error!(
                                        path.span(),
                                        "[const-type-layout]: Invalid #[layout(ground)] \
                                         attribute: structs do not have a ground layout."
                                    ),
                                    syn::Data::Union(syn::DataUnion {
                                        fields: syn::FieldsNamed { named: fields, .. },
                                        ..
                                    }) => {
                                        let mut found = false;

                                        for field in fields {
                                            if field.ident.as_ref() == Some(&g) {
                                                found = true;
                                            }
                                        }

                                        if found {
                                            if ground.is_some() {
                                                emit_error!(
                                                    path.span(),
                                                    "[const-type-layout]: Duplicate \
                                                     #[layout(ground)] attribute."
                                                );
                                            } else {
                                                ground = Some(g);
                                            }
                                        } else {
                                            emit_error!(
                                                path.span(),
                                                "[const-type-layout]: Invalid #[layout(ground)] \
                                                 attribute: \"{}\" is not a field in this union.",
                                                g
                                            );
                                        }
                                    },
                                    syn::Data::Enum(syn::DataEnum { variants, .. }) => {
                                        let mut found = false;

                                        for variant in variants {
                                            if variant.ident == g {
                                                found = true;
                                            }
                                        }

                                        if found {
                                            if ground.is_some() {
                                                emit_error!(
                                                    path.span(),
                                                    "[const-type-layout]: Duplicate \
                                                     #[layout(ground)] attribute."
                                                );
                                            } else {
                                                ground = Some(g);
                                            }
                                        } else {
                                            emit_error!(
                                                path.span(),
                                                "[const-type-layout]: Invalid #[layout(ground)] \
                                                 attribute: \"{}\" is not a variant in this enum.",
                                                g
                                            );
                                        }
                                    },
                                },
                                Err(err) => emit_error!(
                                    s.span(),
                                    "[const-type-layout]: Invalid #[layout(bound = \"{}\")] \
                                     attribute: {}.",
                                    match data {
                                        syn::Data::Enum(_) => "variant",
                                        _ => "field",
                                    },
                                    err
                                ),
                            }
                        } else {
                            emit_error!(
                                path.span(),
                                "[const-type-layout]: Unknown attribute, use `bound`, `free`, or \
                                 `ground`."
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

    if ground.is_none() {
        match data {
            syn::Data::Struct(_) => (),
            syn::Data::Union(syn::DataUnion {
                union_token,
                fields: syn::FieldsNamed { named: fields, .. },
            }) => {
                if fields.len() == 1 {
                    if let Some(syn::Field {
                        ident: Some(field), ..
                    }) = fields.first()
                    {
                        ground = Some(field.clone());
                    }
                }

                if ground.is_none() {
                    emit_error!(
                        union_token.span(),
                        "[const-type-layout]: multi-field unions require an explicit \
                         #[layout(ground = \"field\")] attribute."
                    );
                }
            },
            syn::Data::Enum(syn::DataEnum {
                enum_token,
                variants,
                ..
            }) => {
                if variants.len() == 1 {
                    if let Some(variant) = variants.first() {
                        ground = Some(variant.ident.clone());
                    }
                }

                for variant in variants {
                    if matches!(variant.fields, syn::Fields::Unit) {
                        ground = Some(variant.ident.clone());
                    }
                }

                if ground.is_none() && !variants.is_empty() {
                    emit_error!(
                        enum_token.span(),
                        "[const-type-layout]: multi-variant enums without any unit variant \
                         require an explicit #[layout(ground = \"variant\")] attribute."
                    );
                }
            },
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
        ground,
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
    type_graph_input_generics: syn::Generics,
}

fn generate_generics(
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    generics: &syn::Generics,
    is_enum: bool,
    extra_bounds: &[syn::WherePredicate],
    type_params: &[&syn::Ident],
) -> Generics {
    let mut type_layout_input_generics = generics.clone();
    let mut type_graph_input_generics = generics.clone();

    if is_enum {
        type_layout_input_generics
            .make_where_clause()
            .predicates
            .push(syn::parse_quote! {
                [u8; ::core::mem::size_of::<::core::mem::Discriminant<#ty_name #ty_generics>>()]:
            });

        type_graph_input_generics
            .make_where_clause()
            .predicates
            .push(syn::parse_quote! {
                [u8; ::core::mem::size_of::<::core::mem::Discriminant<#ty_name #ty_generics>>()]:
            });
    }

    for ty in type_params {
        type_layout_input_generics
            .make_where_clause()
            .predicates
            .push(syn::parse_quote! {
                #ty: ~const ::const_type_layout::TypeLayout
            });

        type_graph_input_generics
            .make_where_clause()
            .predicates
            .push(syn::parse_quote! {
                #ty: ~const ::const_type_layout::TypeLayout
            });

        type_graph_input_generics
            .make_where_clause()
            .predicates
            .push(syn::parse_quote! {
                #ty: ~const ::const_type_layout::TypeGraph
            });
    }

    for bound in extra_bounds {
        type_layout_input_generics
            .make_where_clause()
            .predicates
            .push(bound.clone());

        type_graph_input_generics
            .make_where_clause()
            .predicates
            .push(bound.clone());
    }

    Generics {
        type_layout_input_generics,
        type_graph_input_generics,
    }
}

fn layout_of_type(
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    data: &syn::Data,
    reprs: &str,
) -> proc_macro2::TokenStream {
    match data {
        syn::Data::Struct(data) => {
            let fields = quote_structlike_fields(ty_name, ty_generics, &data.fields, false);

            quote! {
                ::const_type_layout::TypeStructure::Struct { repr: #reprs, fields: &[#(#fields),*] }
            }
        },
        syn::Data::Enum(r#enum) => {
            let variants = quote_enum_variants(ty_name, ty_generics, r#enum);

            quote! {
                ::const_type_layout::TypeStructure::Enum { repr: #reprs, variants: &[#(#variants),*] }
            }
        },
        syn::Data::Union(union) => {
            let fields = quote_structlike_fields(
                ty_name,
                ty_generics,
                &syn::Fields::Named(union.fields.clone()),
                true,
            );

            quote! {
                ::const_type_layout::TypeStructure::Union { repr: #reprs, fields: &[#(#fields),*] }
            }
        },
    }
}

fn quote_structlike_fields(
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    fields: &syn::Fields,
    is_union: bool,
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
                    quote_structlike_field_offset(ty_name, ty_generics, &field_name, is_union);

                quote_spanned! { field.span() =>
                    ::const_type_layout::Field {
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
                    quote_structlike_field_offset(ty_name, ty_generics, &field_name, is_union);

                quote_spanned! { field.span() =>
                    ::const_type_layout::Field {
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
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    field_name: &impl quote::ToTokens,
    is_union: bool,
) -> proc_macro2::TokenStream {
    let extra_fields = if is_union { quote!() } else { quote!(..) };

    quote! {
        ::const_type_layout::struct_field_offset!(#ty_name => #ty_name #ty_generics => (*base_ptr).#field_name => #extra_fields)
    }
}

fn quote_enum_variants(
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    r#enum: &syn::DataEnum,
) -> Vec<proc_macro2::TokenStream> {
    r#enum
        .variants
        .iter()
        .map(|variant| {
            let variant_name = &variant.ident;
            let variant_name_str = Literal::string(&variant_name.to_string());

            let fields = quote_variant_fields(ty_name, ty_generics, variant_name, &variant.fields);

            let discriminant_bytes =
                quote_discriminant_bytes(ty_name, ty_generics, variant_name, &variant.fields);

            quote! {
                ::const_type_layout::Variant {
                    name: #variant_name_str,
                    discriminant: ::const_type_layout::Discriminant {
                        big_endian_bytes: #discriminant_bytes,
                    },
                    fields: &[#(#fields),*],
                }
            }
        })
        .collect::<Vec<_>>()
}

fn quote_variant_fields(
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    variant_name: &syn::Ident,
    variant_fields: &syn::Fields,
) -> Vec<proc_macro2::TokenStream> {
    match variant_fields {
        syn::Fields::Named(syn::FieldsNamed { named: fields, .. }) => {
            let field_descriptors = fields
                .pairs()
                .map(|pair| {
                    let syn::Field {
                        ident: field_name,
                        colon_token: field_colon,
                        ty: field_ty,
                        ..
                    } = pair.value();
                    let field_comma = pair.punct();

                    quote!(#field_name #field_colon #field_ty #field_comma)
                })
                .collect::<Vec<_>>();

            fields.iter().map(|field| {
                let field_name_str = Literal::string(&field.ident.as_ref().unwrap().to_string());
                let field_index = &field.ident;
                let field_ty = &field.ty;

                quote_spanned! { field.span() =>
                    ::const_type_layout::Field {
                        name: #field_name_str,
                        offset: ::const_type_layout::struct_variant_field_offset!(
                            #ty_name => #ty_name #ty_generics => #variant_name { #(#field_descriptors)* } => #field_index
                        ),
                        ty: ::core::any::type_name::<#field_ty>(),
                    }
                }
            }).collect()
        },
        syn::Fields::Unnamed(syn::FieldsUnnamed {
            unnamed: fields, ..
        }) => {
            let field_descriptors = fields
                .pairs()
                .map(|pair| {
                    let syn::Field { ty: field_ty, .. } = pair.value();
                    let field_comma = pair.punct();

                    quote!(#field_ty #field_comma)
                })
                .collect::<Vec<_>>();

            fields.iter().enumerate().map(|(field_index, field)| {
                let field_name_str = Literal::string(&field_index.to_string());
                let field_index = syn::Index::from(field_index);
                let field_ty = &field.ty;

                quote_spanned! { field.span() =>
                    ::const_type_layout::Field {
                        name: #field_name_str,
                        offset: ::const_type_layout::struct_variant_field_offset!(
                            #ty_name => #ty_name #ty_generics => #variant_name(#(#field_descriptors)*) => #field_index
                        ),
                        ty: ::core::any::type_name::<#field_ty>(),
                    }
                }
            }).collect()
        },
        syn::Fields::Unit => vec![],
    }
}

fn quote_discriminant_bytes(
    ty_name: &syn::Ident,
    ty_generics: &syn::TypeGenerics,
    variant_name: &syn::Ident,
    variant_fields: &syn::Fields,
) -> proc_macro2::TokenStream {
    let variant_descriptor = match variant_fields {
        syn::Fields::Named(syn::FieldsNamed { named: fields, .. }) => {
            let field_descriptors = fields
                .pairs()
                .map(|pair| {
                    let syn::Field {
                        ident: field_name,
                        colon_token: field_colon,
                        ty: field_ty,
                        ..
                    } = pair.value();
                    let field_comma = pair.punct();

                    quote!(#field_name #field_colon #field_ty #field_comma)
                })
                .collect::<Vec<_>>();

            quote!(#variant_name { #(#field_descriptors)* })
        },
        syn::Fields::Unnamed(syn::FieldsUnnamed {
            unnamed: fields, ..
        }) => {
            let field_descriptors = fields
                .pairs()
                .map(|pair| {
                    let syn::Field { ty: field_ty, .. } = pair.value();
                    let field_comma = pair.punct();

                    quote!(#field_ty #field_comma)
                })
                .collect::<Vec<_>>();

            quote!(#variant_name(#(#field_descriptors)*))
        },
        syn::Fields::Unit => quote!(#variant_name),
    };

    quote! {
        &::const_type_layout::struct_variant_discriminant!(
            #ty_name => #ty_name #ty_generics => #variant_descriptor
        )
    }
}

fn uninit_for_type(
    ty_name: &syn::Ident,
    data: &syn::Data,
    ground: Option<&syn::Ident>,
) -> proc_macro2::TokenStream {
    match data {
        syn::Data::Struct(data) => {
            let fields = match &data.fields {
                syn::Fields::Named(syn::FieldsNamed { named: fields, .. })
                | syn::Fields::Unnamed(syn::FieldsUnnamed {
                    unnamed: fields, ..
                }) => fields,
                syn::Fields::Unit => return quote!(#ty_name),
            };

            let initialisers = fields
                .pairs()
                .map(|pair| {
                    let syn::Field {
                        ident: field_name,
                        colon_token: field_colon,
                        ty: field_ty,
                        ..
                    } = pair.value();
                    let field_comma = pair.punct();

                    quote! {
                        #field_name #field_colon ::core::mem::ManuallyDrop::into_inner(
                            <#field_ty as ::const_type_layout::TypeLayout>::uninit()
                        ) #field_comma
                    }
                })
                .collect::<Vec<_>>();

            if let syn::Fields::Named(_) = &data.fields {
                quote!(#ty_name { #(#initialisers)* })
            } else {
                quote!(#ty_name ( #(#initialisers)* ))
            }
        },
        syn::Data::Enum(r#enum) => {
            let Some(syn::Variant {
                ident: variant_name,
                fields,
                ..
            }) = &r#enum.variants.iter().find(|v| Some(&v.ident) == ground) else {
                return quote!(panic!(concat!("cannot construct uninhabited enum ", stringify!(#ty_name))));
            };

            let variant_fields = match fields {
                syn::Fields::Named(syn::FieldsNamed { named: fields, .. })
                | syn::Fields::Unnamed(syn::FieldsUnnamed {
                    unnamed: fields, ..
                }) => fields,
                syn::Fields::Unit => return quote!(#ty_name::#variant_name),
            };

            let initialisers = variant_fields
                .pairs()
                .map(|pair| {
                    let syn::Field {
                        ident: field_name,
                        colon_token: field_colon,
                        ty: field_ty,
                        ..
                    } = pair.value();
                    let field_comma = pair.punct();

                    quote! {
                        #field_name #field_colon ::core::mem::ManuallyDrop::into_inner(
                            <#field_ty as ::const_type_layout::TypeLayout>::uninit()
                        ) #field_comma
                    }
                })
                .collect::<Vec<_>>();

            if let syn::Fields::Named(_) = fields {
                quote!(#ty_name::#variant_name { #(#initialisers)* })
            } else {
                quote!(#ty_name::#variant_name ( #(#initialisers)* ))
            }
        },
        syn::Data::Union(r#union) => {
            let syn::Field {
                ident: field_name,
                colon_token: field_colon,
                ty: field_ty,
                ..
            } = &r#union
                .fields
                .named
                .iter()
                .find(|f| f.ident.as_ref() == ground)
                .expect("ground checked already");

            quote!(#ty_name {
                #field_name #field_colon ::core::mem::ManuallyDrop::into_inner(
                    <#field_ty as ::const_type_layout::TypeLayout>::uninit()
                )
            })
        },
    }
}
