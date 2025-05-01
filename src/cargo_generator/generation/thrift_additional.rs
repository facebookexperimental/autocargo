// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use anyhow::anyhow;
use itertools::Itertools;
use maplit::hashmap;
use pathdiff::diff_paths;
use proc_macro2::Ident;
use proc_macro2::Literal;
use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::quote;
use thrift_compiler::GenContext;

use crate::buck_processing::AutocargoThrift;
use crate::buck_processing::ThriftConfig;
use crate::cargo_generator::GENERATED_PREAMBLE;
use crate::paths::CargoTomlPath;
use crate::paths::PathInFbcode;
use crate::paths::TargetsPath;

pub fn generate_additional_thrift_files(
    targets_path: &TargetsPath,
    cargo_toml_path: &CargoTomlPath,
    thrift_config: &ThriftConfig,
    autocargo_thrift: &AutocargoThrift,
) -> Result<HashMap<PathInFbcode, String>> {
    let path_to_base = diff_paths("", cargo_toml_path.as_dir().as_ref())
        .and_then(|path| path.to_str().map(|s| s.to_owned()))
        .ok_or_else(|| {
            anyhow!(
                "Failed to make a relative path from '' to {:?} \
                    while constructing thrift compiler path_to_base",
                cargo_toml_path.as_dir()
            )
        })?;

    let input = autocargo_thrift
        .thrift_srcs
        .keys()
        .sorted()
        .map(|src| relative_path(targets_path, cargo_toml_path, src))
        .collect::<Result<Vec<_>>>()?;
    let input_type_hint = input.is_empty().then_some(quote!(as [&Path; 0]));

    let types_crate = &autocargo_thrift.options.types_crate;
    let clients_crate = autocargo_thrift.options.clients_crate.iter();
    let services_crate = autocargo_thrift.options.services_crate.iter();

    // Rust specific options passed to the the thrift compiler.
    let thrift_rust_options = autocargo_thrift
        .options
        .more_options
        .iter()
        .filter_map(|(k, v)| match (k.as_str(), v) {
            ("crate_name" | "default_crate_name" | "include_docs", _) => None,
            (_, None) => Some(itertools::Either::Left(k)),
            (_, Some(v)) => Some(itertools::Either::Right(format!("{k}={v}"))),
        })
        .join(",");
    let thrift_rust_options = (!thrift_rust_options.is_empty())
        .then_some(thrift_rust_options)
        .into_iter();

    let include_srcs = match autocargo_thrift.gen_context {
        GenContext::Types => &autocargo_thrift.options.types_include_srcs,
        GenContext::Clients => &autocargo_thrift.options.clients_include_srcs,
        GenContext::Services => &autocargo_thrift.options.services_include_srcs,
        GenContext::Mocks => &None,
    };
    let include_srcs = include_srcs
        .as_deref()
        .unwrap_or_default()
        .split_terminator(':')
        .map(|src| relative_path(targets_path, cargo_toml_path, src))
        .collect::<Result<Vec<_>>>()?;
    let include_srcs = (!include_srcs.is_empty())
        .then_some(include_srcs)
        .into_iter();

    let extra_srcs = match autocargo_thrift.gen_context {
        GenContext::Types => &autocargo_thrift.options.types_extra_srcs,
        GenContext::Clients => &None,
        GenContext::Services => &None,
        GenContext::Mocks => &None,
    };
    let extra_srcs = extra_srcs
        .as_deref()
        .unwrap_or_default()
        .split_terminator(':')
        .map(|src| relative_path(targets_path, cargo_toml_path, src))
        .collect::<Result<Vec<_>>>()?;
    let extra_srcs = (!extra_srcs.is_empty()).then_some(extra_srcs).into_iter();

    let gen_context = Ident::new(
        &format!("{:?}", autocargo_thrift.gen_context),
        Span::call_site(),
    );

    let thrift_build_filename = PathInFbcode::thrift_build_filename();
    let rerun_if_changed = format!("cargo:rerun-if-changed={thrift_build_filename}");

    let cratemap = thrift_config.cratemap_content.lines().sorted().join("\n");
    let cratemap = format!("\"\\\n{cratemap}\n\"").parse::<Literal>().unwrap();

    Ok(hashmap! {
        cargo_toml_path.as_dir().join_to_path_in_fbcode(PathInFbcode::thrift_lib_filename()) => render(quote! {
            ::codegen_includer_proc_macro::include!();
        }),
        cargo_toml_path.as_dir().join_to_path_in_fbcode(thrift_build_filename) => render(quote! {
            use std::env;
            use std::fs;
            use std::path::Path;

            use thrift_compiler::Config;
            use thrift_compiler::GenContext;

            const CRATEMAP: &str = #cratemap;

            #[rustfmt::skip]
            fn main() {
                // Rerun if thrift_build.rs gets rewritten.
                println!(#rerun_if_changed);

                let out_dir = env::var_os("OUT_DIR").expect("OUT_DIR env not provided");
                let cratemap_path = Path::new(&out_dir).join("cratemap");
                fs::write(cratemap_path, CRATEMAP).expect("Failed to write cratemap");

                Config::from_env(GenContext::#gen_context)
                    .expect("Failed to instantiate thrift_compiler::Config")
                    .base_path(#path_to_base)
                    .types_crate(#types_crate)
                    #(
                        .clients_crate(#clients_crate)
                    )*
                    #(
                        .services_crate(#services_crate)
                    )*
                    #(
                        .options(#thrift_rust_options)
                    )*
                    #(
                        .include_srcs([#(#include_srcs),*])
                    )*
                    #(
                        .extra_srcs([#(#extra_srcs),*])
                    )*
                    .run([#(#input),*] #input_type_hint)
                    .expect("Failed while running thrift compilation");
            }
        }),
    })
}

fn relative_path(
    targets_path: &TargetsPath,
    cargo_toml_path: &CargoTomlPath,
    src: impl AsRef<Path>,
) -> Result<String> {
    let absolute_src = targets_path.as_dir().join_to_path_in_fbcode(src);

    diff_paths(absolute_src.as_ref(), cargo_toml_path.as_dir().as_ref())
        .and_then(|path| path.to_str().map(|s| s.to_owned()))
        .ok_or_else(|| {
            anyhow!(
                "Failed to make a relative path from {:?} to {:?} \
                        while constructing thrift compiler input",
                absolute_src,
                cargo_toml_path.as_dir()
            )
        })
}

fn render(content: TokenStream) -> String {
    let file: syn::File = syn::parse2(content).unwrap();
    let code = prettyplease::unparse(&file);
    format!("// {GENERATED_PREAMBLE}\n\n{code}")
}
