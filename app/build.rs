//! Tauri's build step: reads `tauri.conf.json`, embeds the Windows resources
//! (icon, manifest) and generates the ACL/capability code that
//! `tauri::generate_context!` consumes.
//!
//! `tauri.conf.json` lists `../assets/packs` as a bundle resource so a packaged
//! build ships the factory kit. That directory is gitignored (`assets/` holds
//! downloaded samples, not source) and is absent on a fresh clone. tauri-build
//! refuses to proceed if a listed resource path is missing, so this script
//! creates an empty packs folder first. Runtime listing treats a missing or
//! empty folder as "no kits" — see `library::list_dir`.

use std::path::PathBuf;

fn main() {
    ensure_packs_dir();
    generate_control_dispatch();
    tauri_build::build();
}

// The existing typed Rust command functions are the sole command registry.
// Build-time generation fails when a new input lacks a JSON schema instead of
// silently leaving the agent API behind the UI.
fn generate_control_dispatch() {
    use quote::{format_ident, quote};
    println!("cargo:rerun-if-changed=src/commands.rs");
    let source = std::fs::read_to_string("src/commands.rs").expect("command source");
    let parsed = syn::parse_file(&source).expect("valid command source");
    let mut modules = Vec::new();
    let mut arms = Vec::new();
    let mut entries = Vec::new();
    let mut validators = Vec::new();
    for item in parsed.items {
        let syn::Item::Fn(function) = item else {
            continue;
        };
        if !function
            .attrs
            .iter()
            .any(|a| a.path().segments.iter().any(|s| s.ident == "command"))
        {
            continue;
        }
        let ident = &function.sig.ident;
        let name = ident.to_string();
        if name.starts_with("bench_") || name == "subscribe_meters" {
            continue;
        }
        let module = format_ident!("control_{}", ident);
        let mut fields = Vec::new();
        let mut arguments = Vec::new();
        let has_track_id = function.sig.inputs.iter().any(|input| matches!(input, syn::FnArg::Typed(input) if matches!(&*input.pat, syn::Pat::Ident(p) if p.ident == "track_id")));
        for input in &function.sig.inputs {
            let syn::FnArg::Typed(input) = input else {
                panic!("command receiver")
            };
            let syn::Pat::Ident(binding) = &*input.pat else {
                panic!("command argument pattern")
            };
            let field = &binding.ident;
            match field.to_string().as_str() {
                "state" => arguments.push(quote!(app.state::<AppState>())),
                "app" => arguments.push(quote!(app.clone())),
                _ => {
                    let ty = &input.ty;
                    if field == "track"
                        && !has_track_id
                        && matches!(&**ty,syn::Type::Path(p) if p.path.is_ident("usize"))
                    {
                        fields.push(quote!(pub track: Option<usize>, pub track_id: Option<u32>));
                        arguments.push(quote!(crate::control::resolve_track(
                            app,
                            args.track,
                            args.track_id
                        )?));
                    } else {
                        fields.push(quote!(pub #field: #ty));
                        arguments.push(quote!(args.#field));
                    }
                }
            }
        }
        let awaited = function.sig.asyncness.map(|_| quote!(.await));
        let is_result = matches!(&function.sig.output, syn::ReturnType::Type(_, ty)
            if matches!(&**ty, syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Result")));
        let result = if is_result {
            quote!(#ident(#(#arguments),*) #awaited .map_err(|e| mydaw_control_protocol::ControlError::new("COMMAND_FAILED", e))?)
        } else {
            quote!(#ident(#(#arguments),*) #awaited)
        };
        let description = function
            .attrs
            .iter()
            .filter_map(|attribute| {
                if !attribute.path().is_ident("doc") {
                    return None;
                }
                let syn::Meta::NameValue(value) = &attribute.meta else {
                    return None;
                };
                let syn::Expr::Lit(value) = &value.value else {
                    return None;
                };
                let syn::Lit::Str(value) = &value.lit else {
                    return None;
                };
                Some(value.value().trim().to_owned())
            })
            .find(|line| !line.is_empty())
            .unwrap_or_else(|| name.clone());
        modules.push(quote! {
            #[allow(unused_imports, unused_variables)]
            mod #module {
                use super::*;
                #[derive(serde::Deserialize, schemars::JsonSchema)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                pub struct Args { #(#fields,)* }
                pub fn schema() -> serde_json::Value {
                    serde_json::to_value(schemars::schema_for!(Args)).expect("serializable schema")
                }
                pub fn validate(value: serde_json::Value) -> Result<(), mydaw_control_protocol::ControlError> {
                    serde_json::from_value::<Args>(value).map(|_| ()).map_err(|e|
                        mydaw_control_protocol::ControlError::new("INVALID_ARGUMENTS", e.to_string()))
                }
                pub async fn call(app: &tauri::AppHandle, value: serde_json::Value)
                    -> Result<serde_json::Value, mydaw_control_protocol::ControlError> {
                    use tauri::Manager;
                    let args: Args = serde_json::from_value(value).map_err(|e|
                        mydaw_control_protocol::ControlError::new("INVALID_ARGUMENTS", e.to_string()))?;
                    serde_json::to_value(#result).map_err(|e|
                        mydaw_control_protocol::ControlError::new("SERIALIZE", e.to_string()))
                }
            }
        });
        arms.push(quote!(#name => #module::call(app, args).await,));
        validators.push(quote!(#name => #module::validate(args),));
        entries.push(quote!(crate::control::CommandInfo {
            name: #name, description: #description,
            class: crate::control::command_class(#name), schema: #module::schema(),
        }));
    }
    let output = quote! {
        #(#modules)*
        pub(crate) fn control_catalog() -> Vec<crate::control::CommandInfo> { vec![#(#entries),*] }
        pub(crate) async fn control_dispatch(app: &tauri::AppHandle, command: &str, args: serde_json::Value)
            -> Result<serde_json::Value, mydaw_control_protocol::ControlError> {
            match command { #(#arms)* _ => Err(mydaw_control_protocol::ControlError::new("UNKNOWN_COMMAND", command)) }
        }
        pub(crate) fn control_validate(command: &str, args: serde_json::Value)
            -> Result<(), mydaw_control_protocol::ControlError> {
            match command { #(#validators)* _ => Err(mydaw_control_protocol::ControlError::new("UNKNOWN_COMMAND", command)) }
        }
    };
    let destination =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("control_dispatch.rs");
    std::fs::write(destination, output.to_string()).expect("write command registry");
}

fn ensure_packs_dir() {
    let packs = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets")
        .join("packs");
    if packs.is_dir() {
        return;
    }
    std::fs::create_dir_all(&packs).unwrap_or_else(|err| {
        panic!(
            "could not create the factory packs directory {}: {err}",
            packs.display()
        );
    });
}
