//! Proc-macros backing the `jj-gh` config + CLI layering system.
//!
//! Two macros:
//!
//! - [`config_schema!`]: central, lives in `src/config.rs`. Declares every
//!   config-backed field once: storage type, default, optional `#[cli(...)]`
//!   presentation, optional `#[env(...)]` binding. Emits the `Config` struct,
//!   `Default` impl, and serde derives (with `skip_serializing_if = "Option::is_none"`
//!   auto-added on `Option<T>` fields).
//! - [`subcommand_args!`]: co-located with each command. Emits a clap-parsed
//!   input struct and a resolved struct handlers consume.
//!
//! ## `config_schema!` field attrs
//!
//! Each field can be preceded by:
//!
//! - `///` doc comments: forwarded to the emitted Config field.
//! - `#[serde(..)]`: forwarded verbatim. If any such attr touches
//!   `skip_serializing`, the auto-added `skip_serializing_if` is suppressed.
//! - `#[cli(long = "...", short = '_', value_name = "...", flip = "no-...")]`:
//!   stored for [`subcommand_args!`] to consume (not used yet).
//! - `#[env("ENV_KEY", string | path | argv)]`: env-var binding for the
//!   `env_overlay()` helper.
//! - Any other attribute: forwarded verbatim to the emitted Config field.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use std::fmt::Write as _;
use syn::{
    Attribute, Expr, Ident, LitStr, Meta, Token, Type,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

struct Schema {
    fields: Vec<SchemaField>,
}

struct SchemaField {
    docs: Vec<Attribute>,
    passthrough_attrs: Vec<Attribute>,
    deprecation: Option<Deprecation>,
    serde_attrs: Vec<TokenStream2>,
    _cli: Option<TokenStream2>,
    env: Option<EnvSpec>,
    name: Ident,
    ty: Type,
    default: Expr,
}

#[derive(Default)]
struct Deprecation {
    since: Option<LitStr>,
    note: Option<LitStr>,
}

struct EnvSpec {
    key: LitStr,
    kind: EnvKind,
}

enum EnvKind {
    String,
    Path,
    Argv,
}

impl Parse for EnvSpec {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key = input.parse::<LitStr>()?;
        input.parse::<Token![,]>()?;
        let kind_ident = input.parse::<Ident>()?;
        let kind = match kind_ident.to_string().as_str() {
            "string" => EnvKind::String,
            "path" => EnvKind::Path,
            "argv" => EnvKind::Argv,
            _ => {
                return Err(syn::Error::new_spanned(
                    kind_ident,
                    "expected one of: string, path, argv",
                ));
            }
        };
        Ok(EnvSpec { key, kind })
    }
}

impl Parse for Schema {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut fields = Vec::new();
        while !input.is_empty() {
            fields.push(input.parse()?);
        }
        Ok(Schema { fields })
    }
}

impl Parse for SchemaField {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let raw_attrs = input.call(Attribute::parse_outer)?;
        let mut docs = Vec::new();
        let mut passthrough_attrs = Vec::new();
        let mut deprecation = None;
        let mut serde_attrs = Vec::new();
        let mut cli = None;
        let mut env = None;
        for attr in raw_attrs {
            if attr.path().is_ident("doc") {
                docs.push(attr);
            } else if attr.path().is_ident("serde") {
                serde_attrs.push(attr.parse_args::<TokenStream2>()?);
            } else if attr.path().is_ident("cli") {
                cli = Some(attr.parse_args::<TokenStream2>()?);
            } else if attr.path().is_ident("env") {
                env = Some(attr.parse_args::<EnvSpec>()?);
            } else {
                if attr.path().is_ident("deprecated") {
                    deprecation = Some(parse_deprecation(&attr)?);
                }
                passthrough_attrs.push(attr);
            }
        }
        let name = input.parse::<Ident>()?;
        input.parse::<Token![:]>()?;
        let ty = input.parse::<Type>()?;
        input.parse::<Token![=]>()?;
        let default = input.parse::<Expr>()?;
        let _trailing = input.parse::<Option<Token![,]>>()?;
        Ok(SchemaField {
            docs,
            passthrough_attrs,
            deprecation,
            serde_attrs,
            _cli: cli,
            env,
            name,
            ty,
            default,
        })
    }
}

fn parse_deprecation(attr: &Attribute) -> syn::Result<Deprecation> {
    let mut deprecation = Deprecation::default();
    match &attr.meta {
        Meta::Path(_) => {}
        Meta::List(_) => attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("since") {
                deprecation.since = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("note") {
                deprecation.note = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error("expected `since` or `note`"));
            }
            Ok(())
        })?,
        Meta::NameValue(_) => {
            return Err(syn::Error::new_spanned(
                attr,
                "expected #[deprecated] or #[deprecated(since = \"...\", note = \"...\")]",
            ));
        }
    }
    Ok(deprecation)
}

fn is_option(ty: &Type) -> bool {
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
    {
        return seg.ident == "Option";
    }
    false
}

/// Extracts `T` from `Option<T>`. Returns `None` if `ty` is not syntactically
/// `Option<...>` with a single generic argument.
fn option_inner(ty: &Type) -> Option<Type> {
    let Type::Path(p) = ty else {
        return None;
    };
    let seg = p.path.segments.last()?;
    if seg.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    let arg = args.args.first()?;
    let syn::GenericArgument::Type(inner) = arg else {
        return None;
    };
    Some(inner.clone())
}

fn touches_skip_serializing(serde_attrs: &[TokenStream2]) -> bool {
    serde_attrs
        .iter()
        .any(|tt| tt.to_string().contains("skip_serializing"))
}

fn deprecated_key_entries(fields: &[SchemaField]) -> Vec<TokenStream2> {
    fields
        .iter()
        .filter_map(|field| {
            let deprecation = field.deprecation.as_ref()?;
            let key_name = field.name.to_string();
            let key = LitStr::new(&key_name, field.name.span());
            let mut message = format!("`jj-gh.{key_name}` is deprecated");
            if let Some(since) = &deprecation.since {
                write!(message, " since {}", since.value()).expect("writing to String cannot fail");
            }
            if let Some(note) = &deprecation.note {
                write!(message, ": {}", note.value()).expect("writing to String cannot fail");
            }
            let message = LitStr::new(&message, Span::call_site());
            Some(quote!((#key, #message),))
        })
        .collect()
}

#[proc_macro]
#[expect(clippy::too_many_lines)]
pub fn config_schema(input: TokenStream) -> TokenStream {
    let schema = parse_macro_input!(input as Schema);

    let struct_fields = schema.fields.iter().map(|f| {
        let docs = &f.docs;
        let passthrough_attrs = &f.passthrough_attrs;
        let mut serde_attrs = f
            .serde_attrs
            .iter()
            .map(|inner| quote!(#[serde(#inner)]))
            .collect::<Vec<TokenStream2>>();
        if is_option(&f.ty) && !touches_skip_serializing(&f.serde_attrs) {
            serde_attrs.push(quote!(#[serde(skip_serializing_if = "Option::is_none")]));
        }
        let name = &f.name;
        let ty = &f.ty;
        quote! {
            #(#docs)*
            #(#passthrough_attrs)*
            #(#serde_attrs)*
            pub #name: #ty,
        }
    });

    let default_inits = schema.fields.iter().map(|f| {
        let name = &f.name;
        let default = &f.default;
        quote! { #name: #default, }
    });

    let env_fields = schema.fields.iter().filter_map(|f| {
        let env = f.env.as_ref()?;
        let name = &f.name;
        let ty = &f.ty;
        let key = &env.key;
        let read = match env.kind {
            EnvKind::String => quote!(__env_read_string(#key)),
            EnvKind::Path => quote!(__env_read_path(#key)),
            EnvKind::Argv => quote!(__env_read_argv(#key)),
        };
        Some((name.clone(), ty.clone(), read))
    });

    let (env_struct_fields, env_inits) = env_fields
        .map(|(name, ty, read)| {
            let struct_field = quote! {
                #[serde(skip_serializing_if = "Option::is_none")]
                #name: #ty,
            };
            let init = quote! { #name: #read, };
            (struct_field, init)
        })
        .unzip::<_, _, Vec<_>, Vec<_>>();

    let schema_aliases = schema.fields.iter().map(|f| {
        let name = &f.name;
        let ty = &f.ty;
        quote! {
            #[expect(non_camel_case_types)]
            pub type #name = #ty;
        }
    });

    let deprecated_keys = deprecated_key_entries(&schema.fields);

    let expanded = quote! {
        #[derive(::std::fmt::Debug, ::serde::Deserialize, ::serde::Serialize)]
        #[cfg_attr(feature = "schema-validation", derive(::schemars::JsonSchema))]
        #[serde(default)]
        pub struct Config {
            #(#struct_fields)*
        }

        #[allow(clippy::allow_attributes, deprecated)]
        impl ::std::default::Default for Config {
            fn default() -> Self {
                Self {
                    #(#default_inits)*
                }
            }
        }

        /// Per-field storage-type aliases, used by `subcommand_args!` to
        /// compile-time verify that local `#[config]` fields match the
        /// schema's storage type. Do not depend on this module from outside
        /// generated macro output.
        #[doc(hidden)]
        pub mod __schema {
            use super::*;
            #(#schema_aliases)*
        }

        #[doc(hidden)]
        pub static __DEPRECATED_CONFIG_KEYS: &[(&str, &str)] = &[
            #(#deprecated_keys)*
        ];

        /// Snapshot of env-var bindings declared in `config_schema!`.
        ///
        /// Returned value is a serde-serializable overlay suitable for
        /// `figment::providers::Serialized::defaults(env_overlay())`. Unset or
        /// empty env vars produce `None` fields, which are skipped by serde
        /// so they don't clobber lower layers.
        #[must_use]
        pub fn env_overlay() -> impl ::serde::Serialize {
            #[derive(::serde::Serialize)]
            struct __EnvOverlay {
                #(#env_struct_fields)*
            }
            fn __env_read_string(key: &str) -> ::std::option::Option<::std::string::String> {
                ::std::env::var(key).ok().filter(|s| !s.is_empty())
            }
            fn __env_read_path(key: &str) -> ::std::option::Option<::std::path::PathBuf> {
                ::std::env::var_os(key)
                    .filter(|s| !s.is_empty())
                    .map(::std::path::PathBuf::from)
            }
            fn __env_read_argv(key: &str) -> ::std::option::Option<::std::vec::Vec<::std::string::String>> {
                let raw = ::std::env::var(key).ok().filter(|s| !s.is_empty())?;
                ::shell_words::split(&raw).ok().filter(|v| !v.is_empty())
            }
            __EnvOverlay {
                #(#env_inits)*
            }
        }
    };

    expanded.into()
}

struct SubcommandArgs {
    no_globals: bool,
    vis: syn::Visibility,
    name: Ident,
    fields: Vec<ArgField>,
}

struct ArgField {
    docs: Vec<Attribute>,
    passthrough_attrs: Vec<Attribute>,
    arg: Option<Attribute>,
    config: Option<ConfigLink>,
    name: Ident,
    ty: Type,
}

enum ConfigLink {
    /// `#[config]`: storage-field name matches the schema key.
    Same,
    /// `#[config(maps_to = "schema_key")]`: storage-field name renamed for figment.
    Renamed(LitStr),
    /// `#[config(fallback = "schema_key")]`: CLI override at chain top, schema
    /// key holds last-resort default. Resolved field becomes
    /// `EvalWithCfgFallback<T>`; the CLI value does not merge into the schema
    /// key.
    Fallback(LitStr),
}

impl Parse for SubcommandArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let outer_attrs = input.call(Attribute::parse_outer)?;
        let mut no_globals = false;
        for attr in outer_attrs {
            if attr.path().is_ident("no_globals") {
                no_globals = true;
            } else {
                return Err(syn::Error::new_spanned(
                    attr,
                    "unknown subcommand_args struct attribute; expected #[no_globals]",
                ));
            }
        }
        let vis = input.parse::<syn::Visibility>()?;
        input.parse::<Token![struct]>()?;
        let name = input.parse::<Ident>()?;
        let content;
        syn::braced!(content in input);
        let mut fields = Vec::new();
        while !content.is_empty() {
            fields.push(content.parse()?);
        }
        Ok(SubcommandArgs {
            no_globals,
            vis,
            name,
            fields,
        })
    }
}

impl Parse for ArgField {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let raw_attrs = input.call(Attribute::parse_outer)?;
        let mut docs = Vec::new();
        let mut passthrough_attrs = Vec::new();
        let mut arg = Option::<Attribute>::None;
        let mut config = Option::<ConfigLink>::None;
        for attr in raw_attrs {
            if attr.path().is_ident("doc") {
                docs.push(attr);
            } else if attr.path().is_ident("arg") {
                if arg.is_some() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "duplicate #[arg(..)] attribute",
                    ));
                }
                arg = Some(attr);
            } else if attr.path().is_ident("config") {
                if config.is_some() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "duplicate #[config] attribute",
                    ));
                }
                config = Some(parse_config_attr(&attr)?);
            } else {
                passthrough_attrs.push(attr);
            }
        }
        let _vis = input.parse::<syn::Visibility>()?;
        let name = input.parse::<Ident>()?;
        input.parse::<Token![:]>()?;
        let ty = input.parse::<Type>()?;
        let _trailing = input.parse::<Option<Token![,]>>()?;
        Ok(ArgField {
            docs,
            passthrough_attrs,
            arg,
            config,
            name,
            ty,
        })
    }
}

fn parse_config_attr(attr: &Attribute) -> syn::Result<ConfigLink> {
    match &attr.meta {
        syn::Meta::Path(_) => Ok(ConfigLink::Same),
        syn::Meta::List(list) => {
            let mut maps_to = Option::<LitStr>::None;
            let mut fallback = Option::<LitStr>::None;
            list.parse_nested_meta(|nested| {
                if nested.path.is_ident("maps_to") {
                    let value = nested.value()?.parse::<LitStr>()?;
                    maps_to = Some(value);
                    Ok(())
                } else if nested.path.is_ident("fallback") {
                    let value = nested.value()?.parse::<LitStr>()?;
                    fallback = Some(value);
                    Ok(())
                } else {
                    Err(nested.error("expected `maps_to = \"...\"` or `fallback = \"...\"`"))
                }
            })?;
            match (maps_to, fallback) {
                (Some(_), Some(_)) => Err(syn::Error::new_spanned(
                    attr,
                    "`maps_to` and `fallback` are mutually exclusive",
                )),
                (Some(key), None) => Ok(ConfigLink::Renamed(key)),
                (None, Some(key)) => Ok(ConfigLink::Fallback(key)),
                (None, None) => Err(syn::Error::new_spanned(
                    attr,
                    "expected #[config(maps_to = \"...\")] or #[config(fallback = \"...\")]",
                )),
            }
        }
        syn::Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attr,
            "expected #[config], #[config(maps_to = \"...\")], or #[config(fallback = \"...\")]",
        )),
    }
}

fn schema_key_for(field: &ArgField) -> String {
    match &field.config {
        Some(ConfigLink::Renamed(lit) | ConfigLink::Fallback(lit)) => lit.value(),
        _ => field.name.to_string(),
    }
}

#[proc_macro]
#[expect(clippy::too_many_lines)]
pub fn subcommand_args(input: TokenStream) -> TokenStream {
    let SubcommandArgs {
        no_globals,
        vis,
        name,
        fields,
    } = parse_macro_input!(input as SubcommandArgs);

    let input_name = quote::format_ident!("{}Input", name);

    // Validate: every field must have at least one of #[arg] or #[config].
    for f in &fields {
        if f.arg.is_none() && f.config.is_none() {
            return syn::Error::new_spanned(
                &f.name,
                "field needs at least one of #[arg(..)] or #[config(..)]",
            )
            .to_compile_error()
            .into();
        }
    }

    // Input-struct fields (clap parsing target): everything with #[arg(..)].
    let input_fields = fields.iter().filter_map(|f| {
        let arg = f.arg.as_ref()?;
        let docs = &f.docs;
        let passthrough = &f.passthrough_attrs;
        let fname = &f.name;
        let fty = &f.ty;
        // #[config] fields need Option-wrap in input so figment skips when
        // CLI arg absent. Already-Option types stay as-is. We emit the
        // unqualified `Option` so clap's derive recognizes the optional
        // shape (it pattern-matches on the literal ident `Option`).
        let wrapped = if is_option(fty) {
            quote!(#fty)
        } else {
            quote!(Option<#fty>)
        };
        let (input_ty, serde_attr) = match &f.config {
            Some(ConfigLink::Same | ConfigLink::Renamed(_)) => {
                let key = schema_key_for(f);
                (
                    wrapped,
                    quote! {
                        #[serde(rename = #key, skip_serializing_if = "Option::is_none")]
                    },
                )
            }
            // Fallback: CLI value must NOT merge into the schema key (they sit
            // at opposite ends of a precedence chain). Input keeps Option<T>
            // for clap; serde skips entirely.
            Some(ConfigLink::Fallback(_)) => (wrapped, quote! { #[serde(skip)] }),
            None => (quote!(#fty), quote! { #[serde(skip)] }),
        };
        Some(quote! {
            #(#docs)*
            #(#passthrough)*
            #arg
            #serde_attr
            pub #fname: #input_ty,
        })
    });

    // Resolved-struct fields and resolve-body initializers.
    let mut resolved_fields = Vec::new();
    let mut resolve_inits = Vec::new();
    for f in &fields {
        let fname = &f.name;
        let fty = &f.ty;
        let docs = &f.docs;
        match &f.config {
            Some(ConfigLink::Fallback(_)) => {
                // Resolved field becomes EvalWithCfgFallback<T>. Inner T is
                // the CLI field's unwrapped storage type.
                let key = syn::Ident::new(&schema_key_for(f), proc_macro2::Span::call_site());
                let inner = option_inner(fty).unwrap_or(fty.clone());
                resolved_fields.push(quote! {
                    #(#docs)*
                    pub #fname: crate::macro_support::EvalWithCfgFallback<#inner>,
                });
                resolve_inits.push(quote! {
                    #fname: crate::macro_support::EvalWithCfgFallback::new(
                        input.#fname,
                        ::std::clone::Clone::clone(&config.#key),
                    ),
                });
            }
            Some(ConfigLink::Same | ConfigLink::Renamed(_)) => {
                let key = syn::Ident::new(&schema_key_for(f), proc_macro2::Span::call_site());
                resolved_fields.push(quote! {
                    #(#docs)*
                    pub #fname: #fty,
                });
                resolve_inits.push(quote! {
                    #fname: ::std::clone::Clone::clone(&config.#key),
                });
            }
            None => {
                resolved_fields.push(quote! {
                    #(#docs)*
                    pub #fname: #fty,
                });
                resolve_inits.push(quote! { #fname: input.#fname, });
            }
        }
    }

    // Compile-time type checks: each #[config] field's local type must match
    // the schema storage type alias under `crate::macro_support::__schema`.
    let type_checks = fields.iter().filter_map(|f| {
        let _ = f.config.as_ref()?;
        let fty = &f.ty;
        let key = syn::Ident::new(&schema_key_for(f), proc_macro2::Span::call_site());
        let check_name = quote::format_ident!(
            "_CHECK_{}_{}",
            name.to_string().to_uppercase(),
            f.name.to_string().to_uppercase()
        );
        Some(quote! {
            #[expect(dead_code)]
            const #check_name: fn(#fty) -> crate::macro_support::__schema::#key = ::std::convert::identity;
        })
    });

    let (globals_field, globals_init, resolve_extra_param, resolved_derives) = if no_globals {
        (
            quote!(),
            quote!(),
            quote!(),
            quote!(::std::fmt::Debug, ::std::clone::Clone),
        )
    } else {
        (
            quote! {
                /// Resolved globals (remote, askpass, log options, ...).
                pub globals: crate::macro_support::GlobalOpts,
            },
            quote!(globals: ::std::clone::Clone::clone(globals),),
            quote!(, globals: &crate::macro_support::GlobalOpts),
            quote!(::std::fmt::Debug),
        )
    };

    let expanded = quote! {
        #[derive(::std::fmt::Debug, ::clap::Args, ::serde::Serialize)]
        #vis struct #input_name {
            #(#input_fields)*
        }

        #[derive(#resolved_derives)]
        #vis struct #name {
            #globals_field
            #(#resolved_fields)*
        }

        impl #name {
            /// Build the resolved args from the clap-parsed input plus the
            /// merged Config (and resolved globals, except on `#[no_globals]`
            /// structs like `GlobalOpts` itself).
            #[allow(clippy::allow_attributes, deprecated)]
            pub fn resolve(
                input: #input_name,
                config: &crate::macro_support::Config
                #resolve_extra_param
            ) -> Self {
                Self {
                    #globals_init
                    #(#resolve_inits)*
                }
            }
        }

        #(#type_checks)*
    };

    expanded.into()
}
