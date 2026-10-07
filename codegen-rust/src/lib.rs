use convert_case::{Case, Casing};
use gluon_ipc_parser::{CustomType, EnumDef, Field, Interface, Protocol, StructDef, Type};
use quote::{format_ident, quote};
use std::collections::HashSet;
use std::ops::Deref;

pub use gluon_ipc::Derives;

pub mod helpers;

/// the [`ExternalProtocol`] should come from the `EXTERNAL_PROTOCOL` const from the module
/// defined in `rust_module`
pub struct ModuleExternalProtocol {
	pub rust_module: &'static str,
	pub external_protocol: gluon_ipc::ExternalProtocol,
}
pub struct LocalProtocol {
	/// Short module name (e.g. `"test"`, `"types"`), used to match `TypeProxy` prefixes.
	pub module_name: String,
	pub rust_module: String,
	pub protocol: Protocol,
}
#[derive(Clone)]
/// Maps a protocol type name to an existing Rust type that should be used in the public API
/// instead. The crate implementing the protocol must provide `From<WireType> for RustType` and
/// `From<RustType> for WireType` impls. The generated wire types become `pub(crate)`.
pub struct TypeProxy {
	/// `"module::TypeName"` — module is the short name passed to `gen_multiple_modules`
	pub protocol_type_name: String,
	/// The Rust type path to surface in the public API (e.g. `"glam::Vec3"`)
	pub rust_type: String,
	/// Derives that the proxy type implements, so the codegen can propagate them correctly
	/// to structs/enums that contain this type as a field.
	pub derives: Derives,
}
impl Deref for LocalProtocol {
	type Target = Protocol;

	fn deref(&self) -> &Self::Target {
		&self.protocol
	}
}
impl Deref for ModuleExternalProtocol {
	type Target = gluon_ipc::ExternalProtocol;

	fn deref(&self) -> &Self::Target {
		&self.external_protocol
	}
}
#[derive(Clone, Copy)]
pub struct GenCtx<'a> {
	pub curr_protocol: &'a LocalProtocol,
	pub other_local_protocols: &'a [&'a LocalProtocol],
	pub external_protocols: &'a [&'a ModuleExternalProtocol],
	/// Which derives to attempt on generated structs/enums. A derive is only
	/// applied if every field/variant member supports it.
	pub requested_derives: Derives,
	/// Protocol type names mapped to their public Rust proxy types.
	pub type_proxies: &'a [TypeProxy],
	/// Emit `tracing::trace!` calls in generated proxy methods and dispatch arms.
	pub tracing: bool,
}

pub fn gen_module(
	proto: &LocalProtocol,
	other_local_protocols: &[&LocalProtocol],
	external_protocols: &[&ModuleExternalProtocol],
	requested_derives: Derives,
	type_proxies: &[TypeProxy],
	tracing: bool,
) -> proc_macro2::TokenStream {
	let gen_ctx = &GenCtx {
		curr_protocol: proto,
		other_local_protocols,
		external_protocols,
		requested_derives,
		type_proxies,
		tracing,
	};
	let interfaces = proto
		.interfaces
		.iter()
		.map(|(name, interface)| gen_interface(name, interface, gen_ctx));
	let structs = proto
		.structs
		.iter()
		.filter(|(name, _)| find_proxy_by_name(name, gen_ctx).is_none())
		.map(|(_name, def)| gen_struct(def, gen_ctx));
	let enums = proto
		.enums
		.iter()
		.filter(|(name, _)| find_proxy_by_name(name, gen_ctx).is_none())
		.map(|(_name, def)| gen_enum(def, gen_ctx));
	let proxied_structs = proto
		.structs
		.iter()
		.filter(|(name, _)| find_proxy_by_name(name, gen_ctx).is_some())
		.map(|(_name, def)| gen_struct(def, gen_ctx));
	let proxied_enums = proto
		.enums
		.iter()
		.filter(|(name, _)| find_proxy_by_name(name, gen_ctx).is_some())
		.map(|(_name, def)| gen_enum(def, gen_ctx));
	let external_proto_const = gen_external_protocol_def(gen_ctx);
	let tracing_instrument = if gen_ctx.tracing {
		quote! { use tracing::Instrument as _; }
	} else {
		quote! {}
	};
	quote! {
		#![allow(unused, clippy::all, private_bounds, private_interfaces)]
		use gluon_ipc::Convertable as _;
		#tracing_instrument
		#external_proto_const
		#(#structs)*
		#(#enums)*
		#(#interfaces)*
		pub mod proxied {
			use super::*;
			#(#proxied_structs)*
			#(#proxied_enums)*
		}
	}
}
pub fn gen_external_protocol_def(gen_ctx: &GenCtx) -> proc_macro2::TokenStream {
	let types = gen_ctx
		.curr_protocol
		.structs
		.iter()
		.map(|(name, v)| {
			(
				name.clone(),
				struct_supported_derives(v, gen_ctx),
				find_proxy_by_name(name, gen_ctx),
			)
		})
		.chain(gen_ctx.curr_protocol.enums.iter().map(|(name, v)| {
			(
				name.clone(),
				enum_supported_derives(v, gen_ctx),
				find_proxy_by_name(name, gen_ctx),
			)
		}))
		.map(|(name, derives, proxy)| {
			let bits = derives.bits();
			let proxy = proxy
				.map(|v| {
					let name = v
						.rust_type
						.split("::")
						.last()
						.expect("unable to split proxy rust type");
					let v = format!("proxies::{name}");
					quote! {Some(#v)}
				})
				.unwrap_or_else(|| quote! {None});
			quote! {
				gluon_ipc::ExternalGluonType {
					name: #name,
					supported_derives: gluon_ipc::Derives::from_bits_truncate(#bits),
					proxy: #proxy,
				}
			}
		});
	let proxy_reexports = gen_ctx
		.curr_protocol
		.structs
		.iter()
		.map(|(name, _)| find_proxy_by_name(name, gen_ctx))
		.chain(
			gen_ctx
				.curr_protocol
				.enums
				.iter()
				.map(|(name, _)| find_proxy_by_name(name, gen_ctx)),
		)
		.flatten()
		.map(|proxy| {
			let fragments = proxy.rust_type.split("::").map(|v| format_ident!("{}", v));
			quote! {pub use #(#fragments)::*;}
		});
	let proto_name = &gen_ctx.curr_protocol.name;
	quote! {
		pub const EXTERNAL_PROTOCOL: gluon_ipc::ExternalProtocol = gluon_ipc::ExternalProtocol {
			protocol_name: #proto_name,
			types: &[#(#types),*],
		};
		pub mod proxies {
			use super::*;
			#(#proxy_reexports)*
		}
	}
}
fn proxy_matches(p: &TypeProxy, type_name: &str, gen_ctx: &GenCtx) -> bool {
	match p.protocol_type_name.split_once("::") {
		Some((prefix, name)) => prefix == gen_ctx.curr_protocol.module_name && name == type_name,
		None => panic!(
			"TypeProxy::protocol_type_name must be \"module::TypeName\", got {:?}",
			p.protocol_type_name
		),
	}
}

/// Finds the registered proxy for a bare type name within the current protocol.
fn find_proxy_by_name<'a>(type_name: &str, gen_ctx: &'a GenCtx) -> Option<&'a TypeProxy> {
	gen_ctx
		.type_proxies
		.iter()
		.find(|p| proxy_matches(p, type_name, gen_ctx))
}

/// Finds the registered proxy for a qualified type (`namespace::TypeName`), resolving the import
/// alias to the target protocol's `module_name` before matching.
fn find_proxy_for_qualified(
	namespace: &str,
	type_name: &str,
	gen_ctx: &GenCtx,
) -> Option<TypeProxy> {
	let import = gen_ctx
		.curr_protocol
		.imports
		.iter()
		.find(|v| v.alias == namespace)?;
	if let Some(target) = gen_ctx
		.other_local_protocols
		.iter()
		.find(|v| v.name == import.name)
	{
		gen_ctx
			.type_proxies
			.iter()
			.find(|p| match p.protocol_type_name.split_once("::") {
				Some((prefix, name)) => prefix == target.module_name && name == type_name,
				None => panic!(
					"TypeProxy::protocol_type_name must be \"module::TypeName\", got {:?}",
					p.protocol_type_name
				),
			})
			.cloned()
	} else {
		let target = gen_ctx
			.external_protocols
			.iter()
			.find(|v| v.protocol_name == import.name)?;
		let ty = target.types.iter().find(|v| v.name == type_name)?;
		Some(TypeProxy {
			protocol_type_name: type_name.to_string(),
			rust_type: format!("{}::{}", target.rust_module, ty.proxy?),
			derives: ty.supported_derives,
		})
	}
}

/// Finds the registered proxy for a `Type`, returning its parsed `TokenStream`.
fn find_proxy(ty: &Type, gen_ctx: &GenCtx) -> Option<proc_macro2::TokenStream> {
	let proxy = match ty {
		Type::Custom(CustomType::Named(type_name)) => {
			find_proxy_by_name(type_name, gen_ctx).cloned()
		}
		Type::Custom(CustomType::Qualified(namespace, type_name)) => {
			find_proxy_for_qualified(namespace, type_name, gen_ctx)
		}
		_ => None,
	};
	proxy.map(|p| {
		p.rust_type
			.parse()
			.expect("TypeProxy::rust_type is not a valid token stream")
	})
}

/// Like `gen_type` but substitutes the proxy rust type when one is registered,
/// recursing into container types (Option, Vec, Array, Set, Map, Result).
fn gen_public_type(ty: &Type, gen_ctx: &GenCtx) -> proc_macro2::TokenStream {
	if let Some(proxy_ts) = find_proxy(ty, gen_ctx) {
		return proxy_ts;
	}
	match ty {
		Type::Option(inner) => {
			let inner = gen_public_type(inner, gen_ctx);
			quote! { Option<#inner> }
		}
		Type::Vec(inner) => {
			let inner = gen_public_type(inner, gen_ctx);
			quote! { Vec<#inner> }
		}
		Type::Array(inner, len) => {
			let inner = gen_public_type(inner, gen_ctx);
			quote! { [#inner; #len] }
		}
		Type::Set(inner) => {
			let inner = gen_public_type(inner, gen_ctx);
			quote! { std::collections::HashSet<#inner> }
		}
		Type::Map(k, v) => {
			let k = gen_public_type(k, gen_ctx);
			let v = gen_public_type(v, gen_ctx);
			quote! { std::collections::HashMap<#k, #v> }
		}
		Type::Result(ok, err) => {
			let ok = gen_public_type(ok, gen_ctx);
			let err = gen_public_type(err, gen_ctx);
			quote! { Result<#ok, #err> }
		}
		_ => gen_type(ty, gen_ctx),
	}
}

/// Returns true if `ty` contains a proxied type at any nesting depth.
fn type_has_proxy(ty: &Type, gen_ctx: &GenCtx) -> bool {
	if find_proxy(ty, gen_ctx).is_some() {
		return true;
	}
	match ty {
		Type::Option(inner) | Type::Vec(inner) | Type::Array(inner, _) | Type::Set(inner) => {
			type_has_proxy(inner, gen_ctx)
		}
		Type::Map(k, v) => type_has_proxy(k, gen_ctx) || type_has_proxy(v, gen_ctx),
		Type::Result(ok, err) => type_has_proxy(ok, gen_ctx) || type_has_proxy(err, gen_ctx),
		_ => false,
	}
}

/// Generates an expression that converts an owned wire-typed value (`expr`) to the public type.
fn gen_wire_to_pub(
	ty: &Type,
	expr: proc_macro2::TokenStream,
	gen_ctx: &GenCtx,
) -> proc_macro2::TokenStream {
	if find_proxy(ty, gen_ctx).is_some() {
		return quote! { #expr.into() };
	}
	match ty {
		Type::Option(inner) if type_has_proxy(inner, gen_ctx) => {
			let conv = gen_wire_to_pub(inner, quote! { __v }, gen_ctx);
			quote! { #expr.map(|__v| #conv) }
		}
		Type::Vec(inner) if type_has_proxy(inner, gen_ctx) => {
			let conv = gen_wire_to_pub(inner, quote! { __v }, gen_ctx);
			quote! { #expr.into_iter().map(|__v| #conv).collect() }
		}
		Type::Array(inner, _) if type_has_proxy(inner, gen_ctx) => {
			let conv = gen_wire_to_pub(inner, quote! { __v }, gen_ctx);
			quote! { #expr.map(|__v| #conv) }
		}
		Type::Set(inner) if type_has_proxy(inner, gen_ctx) => {
			let conv = gen_wire_to_pub(inner, quote! { __v }, gen_ctx);
			quote! { #expr.into_iter().map(|__v| #conv).collect() }
		}
		_ => expr,
	}
}

/// Generates an expression that converts an owned public-typed value (`expr`) to the wire type.
fn gen_pub_to_wire(
	ty: &Type,
	expr: proc_macro2::TokenStream,
	gen_ctx: &GenCtx,
) -> proc_macro2::TokenStream {
	if find_proxy(ty, gen_ctx).is_some() {
		return quote! { #expr.into() };
	}
	match ty {
		Type::Option(inner) if type_has_proxy(inner, gen_ctx) => {
			let conv = gen_pub_to_wire(inner, quote! { __v }, gen_ctx);
			quote! { #expr.map(|__v| #conv) }
		}
		Type::Vec(inner) if type_has_proxy(inner, gen_ctx) => {
			let conv = gen_pub_to_wire(inner, quote! { __v }, gen_ctx);
			quote! { #expr.into_iter().map(|__v| #conv).collect() }
		}
		Type::Array(inner, _) if type_has_proxy(inner, gen_ctx) => {
			let conv = gen_pub_to_wire(inner, quote! { __v }, gen_ctx);
			quote! { #expr.map(|__v| #conv) }
		}
		Type::Set(inner) if type_has_proxy(inner, gen_ctx) => {
			let conv = gen_pub_to_wire(inner, quote! { __v }, gen_ctx);
			quote! { #expr.into_iter().map(|__v| #conv).collect() }
		}
		_ => expr,
	}
}

pub fn gen_interface(
	interface_name: &str,
	def: &Interface,
	gen_ctx: &GenCtx,
) -> proc_macro2::TokenStream {
	let name = format_ident!("{}", interface_name.to_case(Case::Pascal));
	let handler_name = format_ident!("{name}Handler");
	let local_name = format_ident!("{name}Local");
	let interface_id = format!(
		"{}.{}",
		gen_ctx.curr_protocol.name,
		interface_name.to_case(Case::Pascal)
	);
	let interface_doc_comment = {
		let v = &def.doc;
		quote! {#[doc = #v]}
	};
	let handler = {
		// Dispatch arms: read wire types from the binder, convert to proxy types for the handler
		// call, then convert return values back to wire types for the response.
		let methods_dispatch = def.methods.iter().enumerate().map(|(i, method)| {
            let i = i + 8;
            let names = method.params.iter().map(|v| format_ident!("param_{}", v.name.to_case(Case::Snake))).collect::<Vec<_>>();
            // For proxy params, read wire value into a separate __wire_ var so it can be
            // traced (Debug) before converting to the proxy type (unknown Debug).
            let params_reads = names.iter().zip(method.params.iter()).map(|(var, param)| {
                if type_has_proxy(&param.ty, gen_ctx) {
                    let wire_ty = gen_type(&param.ty, gen_ctx);
                    let wire_var = format_ident!("__wire_{}", var);
                    quote! { let #wire_var: #wire_ty = gluon_ipc::Convertable::read(&mut gluon_data)?; }
                } else {
                    quote! { let #var = gluon_ipc::Convertable::read(&mut gluon_data)?; }
                }
            }).collect::<Vec<_>>();
            let params_converts = names.iter().zip(method.params.iter()).map(|(var, param)| {
                if type_has_proxy(&param.ty, gen_ctx) {
                    let pub_ty = gen_public_type(&param.ty, gen_ctx);
                    let wire_var = format_ident!("__wire_{}", var);
                    let conv = gen_wire_to_pub(&param.ty, quote! { __w }, gen_ctx);
                    quote! { let #var: #pub_ty = { let __w = #wire_var; #conv }; }
                } else {
                    quote! {}
                }
            }).collect::<Vec<_>>();
            let name = format_ident!("{}", method.name.to_case(Case::Snake));
            let method_str = method.name.as_str();
            let dispatch_trace = if gen_ctx.tracing {
                let trace_fields = names.iter().zip(method.params.iter()).map(|(var, param)| {
                    if type_has_proxy(&param.ty, gen_ctx) {
                        let wire_var = format_ident!("__wire_{}", var);
                        quote! { #var = ?#wire_var, }
                    } else {
                        quote! { ?#var, }
                    }
                });
                quote! { tracing::trace!(interface = #interface_name, method = #method_str, #(#trace_fields)* "dispatching"); }
            } else {
                quote! {}
            };
            let return_names = method.returns.as_ref().map(|v| {
                v.iter()
                    .map(|v| format_ident!("{}", v.name.to_case(Case::Snake)))
                    .collect::<Vec<_>>()
            });
            let i = i as u32;
            let tracing_span_instrument = if gen_ctx.tracing {
                quote! { .instrument(tracing::trace_span!("dispatching", interface = #interface_name, method = #method_str, method_id = #i)) }
            } else {
                quote! {}
            };
            if let Some(ref return_names) = return_names {
                let return_defs = method.returns.as_ref().unwrap();
                let oneway_name = format_ident!("{}_oneway", name);
                let return_pub_type = {
                    let types = return_defs.iter().map(|v| gen_public_type(&v.ty, gen_ctx)).collect::<Vec<_>>();
                    match types.as_slice() {
                        [] => quote! {()},
                        [ty] => quote! {#ty},
                        types => quote! {(#(#types),*)},
                    }
                };
                let return_pattern = match return_names.as_slice() {
                    [] => quote! {()},
                    [single] => quote! {#single},
                    names => quote! {(#(#names),*)},
                };
                // Conversion + wire-writing logic for the reply value, handed to `ReplySender`
                // as a plain (non-capturing) fn so `reply.send(value)` can do the pub->wire
                // conversion generically without the caller ever touching a `DataBuilder`.
                let return_writes = return_names.iter().zip(return_defs.iter()).map(|(ret_name, ret_def)| {
                    if type_has_proxy(&ret_def.ty, gen_ctx) {
                        let wire_ty = gen_type(&ret_def.ty, gen_ctx);
                        let conv = gen_pub_to_wire(&ret_def.ty, quote! { #ret_name }, gen_ctx);
                        quote! {
                            let __w: #wire_ty = #conv;
                            __w.write_owned(gluon_out)?;
                        }
                    } else {
                        quote! { #ret_name.write_owned(gluon_out)?; }
                    }
                }).collect::<Vec<_>>();
                let dispatch_return_trace = if gen_ctx.tracing {
                    let trace_fields = return_names.iter().zip(return_defs.iter()).map(|(ret_name, ret_def)| {
                        if type_has_proxy(&ret_def.ty, gen_ctx) {
                            let type_name = gen_public_type(&ret_def.ty, gen_ctx).to_string().replace(' ', "");
                            quote! { #ret_name = #type_name, }
                        } else {
                            quote! { ?#ret_name, }
                        }
                    });
                    quote! { tracing::trace!(interface = #interface_name, method = #method_str, #(#trace_fields)* "←"); }
                } else {
                    quote! {}
                };
                quote! {
                    #i => {
                        let return_callback = gluon_data.read_ref()?;
                        #(#params_reads)*
                        #dispatch_trace
                        #(#params_converts)*
                        drop(gluon_data);
                        let reply: gluon_ipc::ReplySender<#return_pub_type> = gluon_ipc::ReplySender::new(
                            return_callback,
                            |#return_pattern, gluon_out| {
                                #dispatch_return_trace
                                #(#return_writes)*
                                Ok(())
                            },
                        );
                        self.#oneway_name(ctx, #(#names,)* reply)#tracing_span_instrument.await?;
                    },
                }
            } else {
                quote! {
                    #i => {
                        #(#params_reads)*
                        #dispatch_trace
                        #(#params_converts)*
                        drop(gluon_data);
                        self.#name(ctx, #(#names),*)#tracing_span_instrument.await;
                    },
                }
            }
        });
		// Handler trait: public-facing signatures use proxy types where registered.
		let methods = def.methods.iter().map(|method| {
            let param_names: Vec<proc_macro2::Ident> = method.params.iter()
                .map(|p| format_ident!("{}", p.name.to_case(Case::Snake)))
                .collect();
            let params: Vec<proc_macro2::TokenStream> = method.params.iter().zip(param_names.iter()).map(|(param, pname)| {
                let type_def = gen_public_type(&param.ty, gen_ctx);
                quote! { #pname: #type_def }
            }).collect();
            let name = format_ident!("{}", method.name.to_case(Case::Snake));
            let doc_comment = method.doc.as_ref().map(|str| quote! {#[doc = #str]});
            let return_types = method.returns.as_ref().map(|v| {
                v.iter()
                    .map(|v| gen_public_type(&v.ty, gen_ctx))
                    .collect::<Vec<_>>()
            });
            let fn_return = match return_types.as_deref() {
                None => quote! { -> impl Future<Output=()> + Send + Sync },
                Some(types) => {
                    let types = match types {
                        [] => quote! {()},
                        [ty] => quote! {#ty},
                        types => quote! {(#(#types),*)},
                    };
                    quote! { -> impl Future<Output=#types> + Send + Sync }
                }
            };
            // For methods with a return value, generate a default `_oneway` method:
            // it awaits the real method and sends the result through a `ReplySender`,
            // so an override can instead stash `reply` and return immediately without
            // holding up dispatch of the next transaction.
            let oneway_method = if let Some(ref return_defs) = method.returns {
                let oneway_name = format_ident!("{}_oneway", name);
                let return_pub_type = match return_types.as_deref().unwrap() {
                    [] => quote! {()},
                    [ty] => quote! {#ty},
                    types => quote! {(#(#types),*)},
                };
                let return_names: Vec<proc_macro2::Ident> = return_defs.iter()
                    .map(|v| format_ident!("{}", v.name.to_case(Case::Snake)))
                    .collect();
                let return_pattern = match return_names.as_slice() {
                    [] => quote! {()},
                    [single] => quote! {#single},
                    names => quote! {(#(#names),*)},
                };
                let oneway_doc_comment = {
                    let msg = format!(
                        "Dispatched instead of [`Self::{name}`] so a slow reply doesn't hold up dispatch of the next transaction. \
                         The default implementation just awaits `{name}` and sends the result through `reply`. \
                         Override this method instead of `{name}` to defer the reply: stash `reply` (it's `Send + Sync + 'static`) \
                         somewhere else — a channel, a queue, another task — and return as soon as this method's future is done, \
                         without waiting for the reply to actually be sent."
                    );
                    quote! { #[doc = #msg] }
                };
                quote! {
                    #oneway_doc_comment
                    fn #oneway_name(
                        &self,
                        _ctx: gluon_ipc::Context,
                        #(#params,)*
                        reply: gluon_ipc::ReplySender<#return_pub_type>,
                    ) -> impl Future<Output = Result<(), gluon_ipc::SendError>> + Send + Sync {
                        async move {
                            let #return_pattern = self.#name(_ctx, #(#param_names),*).await;
                            reply.send(#return_pattern)
                        }
                    }
                }
            } else {
                quote! {}
            };
            quote! {
                #doc_comment
                fn #name(&self, _ctx: gluon_ipc::Context, #(#params),*) #fn_return;
                #oneway_method
            }
        });
		quote! {
			#interface_doc_comment
			pub trait #handler_name: gluon_ipc::Handler + Send + Sync + 'static {
				#(#methods)*

				fn dispatch_one_way(&self, transaction_code: u32, mut gluon_data: gluon_ipc::DataReader, ctx: gluon_ipc::Context) -> impl Future<Output=Result<(),gluon_ipc::SendError>> + Send + Sync {
					async move {
						match transaction_code {
							#(#methods_dispatch)*
							_ => {}
						}
						Ok(())
					}
				}

				fn to_node(self) -> Result<(gluon_ipc::Node<Self>, gluon_ipc::LocalRef<#name, Self>), gluon_ipc::NodeError> where Self: Sized {
					use gluon_ipc::RefExt;
					#name::new_node(self)
				}
				fn to_service(self) -> Result<gluon_ipc::LocalRef<#name, Self>, gluon_ipc::NodeError> where Self: Sized {
					use gluon_ipc::RefExt;
					#name::new_service(self)
				}
			}
		}
	};
	let proxy = {
		let methods = def.methods.iter().enumerate().map(|(i, method)| {
            // Params: proxy/nested-proxy types taken directly; untyped refs take &impl ToRef;
            // all others use impl Into<WireType>.
            let param_names: Vec<proc_macro2::Ident> = method.params.iter()
                .map(|p| format_ident!("{}", p.name.to_case(Case::Snake)))
                .collect();
            let params = method.params.iter().zip(param_names.iter()).map(|(param, pname)| {
                if type_has_proxy(&param.ty, gen_ctx) {
                    let pub_ty = gen_public_type(&param.ty, gen_ctx);
                    quote! { #pname: #pub_ty }
                } else if matches!(param.ty, Type::Ref(None)) {
                    quote! { #pname: &impl gluon_ipc::ToRef }
                } else {
                    let wire_ty = gen_type(&param.ty, gen_ctx);
                    quote! { #pname: impl Into<#wire_ty> }
                }
            }).collect::<Vec<_>>();
            // Convert every param to its wire type before writing.
            let params_convert = method.params.iter().zip(param_names.iter()).map(|(param, pname)| {
                let wire_ty = gen_type(&param.ty, gen_ctx);
                if type_has_proxy(&param.ty, gen_ctx) {
                    let conv = gen_pub_to_wire(&param.ty, quote! { #pname }, gen_ctx);
                    quote! { let #pname: #wire_ty = #conv; }
                } else if matches!(param.ty, Type::Ref(None)) {
                    quote! { let #pname: #wire_ty = gluon_ipc::ToRef::to_ref(#pname); }
                } else {
                    quote! { let #pname: #wire_ty = #pname.into(); }
                }
            }).collect::<Vec<_>>();
            let params_write = param_names.iter().map(|pname| {
                quote! { #pname.write(&mut gluon_builder)?; }
            }).collect::<Vec<_>>();
            let name = format_ident!("{}", method.name.to_case(Case::Snake));
            let method_str = method.name.as_str();
            let proxy_trace = if gen_ctx.tracing {
                let trace_fields = param_names.iter().map(|pname| quote! { ?#pname, });
                quote! { tracing::trace!(interface = #interface_name, method = #method_str, #(#trace_fields)* "→"); }
            } else {
                quote! {}
            };
            let doc_comment = method.doc.as_ref().map(|str| quote! {#[doc = #str]});
            // Return types: public (proxy) types in the signature.
            let pub_return_types = method.returns.as_ref().map(|v| {
                v.iter().map(|v| gen_public_type(&v.ty, gen_ctx)).collect::<Vec<_>>()
            });
            let i = i as u32 + 8;
            match pub_return_types {
                Some(ref pub_types) => {
                    let ret_defs = method.returns.as_ref().unwrap();
                    let fn_return = match pub_types.as_slice() {
                        [] => quote! {()},
                        [ty] => quote! {#ty},
                        types => quote! {(#(#types),*)},
                    };
                    // Name each return value so we can trace per-value before returning.
                    let ret_vars: Vec<proc_macro2::Ident> = ret_defs.iter()
                        .map(|r| format_ident!("__ret_{}", r.name.to_case(Case::Snake)))
                        .collect();
                    let ret_let_stmts = ret_vars.iter().zip(ret_defs.iter()).map(|(var, ret_def)| {
                        let base = quote! { gluon_ipc::Convertable::read(&mut reader)? };
                        if type_has_proxy(&ret_def.ty, gen_ctx) {
                            let wire_ty = gen_type(&ret_def.ty, gen_ctx);
                            let conv = gen_wire_to_pub(&ret_def.ty, quote! { __w }, gen_ctx);
                            quote! { let #var = { let __w: #wire_ty = #base; #conv }; }
                        } else {
                            quote! { let #var = #base; }
                        }
                    }).collect::<Vec<_>>();
                    let return_result = match ret_vars.as_slice() {
                        [] => quote! {()},
                        [single] => quote! {#single},
                        vars => quote! {(#(#vars),*)},
                    };
                    let proxy_return_trace = if gen_ctx.tracing {
                        let trace_fields = ret_vars.iter().zip(ret_defs.iter()).map(|(var, ret_def)| {
                            if type_has_proxy(&ret_def.ty, gen_ctx) {
                                let type_name = gen_public_type(&ret_def.ty, gen_ctx).to_string().replace(' ', "");
                                quote! { #var = #type_name, }
                            } else {
                                quote! { ?#var, }
                            }
                        });
                        quote! { tracing::trace!(interface = #interface_name, method = #method_str, #(#trace_fields)* "←"); }
                    } else {
                        quote! {}
                    };
                    quote! {
                        #doc_comment
                        pub async fn #name(&self, #(#params),*) -> Result<#fn_return, gluon_ipc::SendError> {
                            #(#params_convert)*
                            #proxy_trace
                            let mut gluon_builder = gluon_ipc::DataBuilder::new();
                            let (mut gluon_recv, gluon_ret) = gluon_ipc::ReturnReceiver::new()?;
                            gluon_builder.write_ref(&gluon_ret)?;
                            drop(gluon_ret);
                            #(#params_write)*
                            gluon_ipc::transact(&self.obj, #i, gluon_builder)?;
                            // safe since we're also holding the channel sender
                            let mut reader = gluon_recv.recv().await?;
                            #(#ret_let_stmts)*
                            #proxy_return_trace
                            Ok(#return_result)
                        }
                    }
                }
                None => quote! {
                    #doc_comment
                    pub fn #name(&self, #(#params),*) -> Result<(), gluon_ipc::SendError> {
                        #(#params_convert)*
                        #proxy_trace
                        let mut gluon_builder = gluon_ipc::DataBuilder::new();
                        #(#params_write)*
                        gluon_ipc::transact(&self.obj, #i, gluon_builder)?;
                        Ok(())
                    }
                },
            }
        });
		quote! {
			#interface_doc_comment
			#[derive(Debug, Clone)]
			pub struct #name {
				obj: gluon_ipc::Ref,
			}

			impl gluon_ipc::Convertable for #name {
				fn write(&self, gluon_data: &mut gluon_ipc::DataBuilder) -> Result<(), gluon_ipc::WriteError> {
					self.obj.write(gluon_data)
				}

				fn read(gluon_data: &mut gluon_ipc::DataReader) -> Result<Self, gluon_ipc::ReadError> {
					let obj = gluon_ipc::Ref::read(gluon_data)?;
					Ok(#name::from_ref(obj))
				}

				fn write_owned(self, gluon_data: &mut gluon_ipc::DataBuilder) -> Result<(), gluon_ipc::WriteError> {
					self.obj.write_owned(gluon_data)
				}
			}
			impl #name {
				const ID: &'static str = #interface_id;
			}
			impl gluon_ipc::Interface for #name {
				const ID: &'static str = Self::ID;
			}
			#[doc = "Carries the per-interface bound for [`gluon_ipc::RefExt`]'s handler constructors: only a handler implementing this interface's handler trait can be passed to them."]
			impl<H: #handler_name> gluon_ipc::HandledBy<H> for #name {}
			#[doc = "A proxy this process made, carrying the handler behind it — see [`gluon_ipc::LocalRef`]. Handed back by [`gluon_ipc::RefExt::new_node`] and [`gluon_ipc::RefExt::new_service`]."]
			pub type #local_name<H> = gluon_ipc::LocalRef<#name, H>;
			#[doc = "Drops the handler share and keeps the proxy, so a [`gluon_ipc::LocalRef`] goes anywhere this proxy does — including the `impl Into<Self>` parameters generated for typed refs."]
			impl<H: #handler_name> From<#local_name<H>> for #name {
				fn from(value: #local_name<H>) -> #name {
					value.into_proxy()
				}
			}
			impl gluon_ipc::RefExt for #name {
				fn from_ref(obj: gluon_ipc::Ref) -> #name {
					#name {
						obj,
					}
				}
			}
			impl #name {
				#(#methods)*
				#[doc = "only use this when you know the ref leads to something implementing this interface, else the consquences are for you to find out"]
				pub fn from_ref(obj: gluon_ipc::Ref) -> #name {
					#name {
						obj,
					}
				}
			}
			impl From<#name> for gluon_ipc::Ref {
				fn from(value: #name) -> Self {
					value.obj
				}
			}
			impl gluon_ipc::ToRef for #name {
				fn to_ref(&self) -> gluon_ipc::Ref {
					self.obj.clone()
				}
			}
			impl gluon_ipc::Liveness for #name {
				fn death_notifier(&self) -> gluon_ipc::DeathNotifier {
					gluon_ipc::Liveness::death_notifier(&self.obj)
				}
			}
			impl std::hash::Hash for #name {
				fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
					self.obj.hash(state);
				}
			}
			impl PartialEq for #name {
				fn eq(&self, other: &Self) -> bool {
					self.obj == other.obj
				}
			}
			impl Eq for #name {}
		}
	};
	quote! {
		#proxy
		#handler
	}
}

/// Returns true if `ty` transitively contains `target` as an inline (non-heap-allocated) type.
/// Vec/Set/Map/Ref are heap-allocated and break the cycle; Option/Result/Array are inline.
fn type_is_recursive(ty: &Type, target: &str, gen_ctx: &GenCtx) -> bool {
	let mut visiting = HashSet::new();
	type_contains_inline(ty, target, gen_ctx, &mut visiting)
}

fn type_contains_inline(
	ty: &Type,
	target: &str,
	gen_ctx: &GenCtx,
	visiting: &mut HashSet<String>,
) -> bool {
	match ty {
		Type::Bool
		| Type::U8
		| Type::U16
		| Type::U32
		| Type::U64
		| Type::I8
		| Type::I16
		| Type::I32
		| Type::I64
		| Type::F32
		| Type::F64
		| Type::String
		| Type::Fd => false,
		// heap-allocated or pointer — break any recursion cycle
		Type::Vec(_) | Type::Set(_) | Type::Map(_, _) | Type::Ref(_) => false,
		// inline wrappers — propagate
		Type::Option(inner) | Type::Array(inner, _) => {
			type_contains_inline(inner, target, gen_ctx, visiting)
		}
		Type::Result(ok, err) => {
			type_contains_inline(ok, target, gen_ctx, visiting)
				|| type_contains_inline(err, target, gen_ctx, visiting)
		}
		Type::Custom(CustomType::Qualified(_, _)) => false, // external type, safe
		Type::Custom(CustomType::Named(name)) => {
			if name == target {
				return true;
			}
			if visiting.contains(name) {
				return false;
			}
			visiting.insert(name.clone());
			let result = named_type_contains_inline(name, target, gen_ctx, visiting);
			visiting.remove(name);
			result
		}
	}
}

fn named_type_contains_inline(
	name: &str,
	target: &str,
	gen_ctx: &GenCtx,
	visiting: &mut HashSet<String>,
) -> bool {
	let proto = &gen_ctx.curr_protocol.protocol;
	if let Some((_, s)) = proto.structs.iter().find(|(n, _)| n == name) {
		return s
			.fields
			.iter()
			.any(|f| type_contains_inline(&f.ty, target, gen_ctx, visiting));
	}
	if let Some((_, e)) = proto.enums.iter().find(|(n, _)| n == name) {
		return e
			.variants
			.iter()
			.flat_map(|v| v.fields.iter())
			.any(|f| type_contains_inline(&f.ty, target, gen_ctx, visiting));
	}
	false
}

pub fn gen_struct(def: &StructDef, gen_ctx: &GenCtx) -> proc_macro2::TokenStream {
	let fields = def
		.fields
		.iter()
		.map(|f| gen_field_struct(f, gen_ctx, type_is_recursive(&f.ty, &def.name, gen_ctx)));
	let name = def.name.to_case(Case::Pascal);
	let derives = derives_to_tokens(struct_supported_derives(def, gen_ctx));
	let serde_derives = derives_to_serde_tokens(struct_supported_derives(def, gen_ctx));
	let name = format_ident!("{}", name);
	let doc = &def.doc;
	let gluon_trait_impl = {
		let field_names = def
			.fields
			.iter()
			.map(|v| format_ident!("{}", v.name.to_case(Case::Snake)))
			.collect::<Vec<_>>();
		let writes = def.fields.iter().map(|f| {
			let fname = format_ident!("{}", f.name.to_case(Case::Snake));
			if type_has_proxy(&f.ty, gen_ctx) {
				let wire_ty = gen_type(&f.ty, gen_ctx);
				let conv = gen_pub_to_wire(&f.ty, quote! { self.#fname.clone() }, gen_ctx);
				quote! { { let __w: #wire_ty = #conv; __w.write_owned(gluon_data)?; } }
			} else {
				quote! { self.#fname.write(gluon_data)?; }
			}
		});
		let reads = def.fields.iter().map(|f| {
			let fname = format_ident!("{}", f.name.to_case(Case::Snake));
			if type_has_proxy(&f.ty, gen_ctx) {
				let wire_ty = gen_type(&f.ty, gen_ctx);
				let pub_ty = gen_public_type(&f.ty, gen_ctx);
				let conv = gen_wire_to_pub(&f.ty, quote! { __w }, gen_ctx);
				quote! {
					let #fname: #pub_ty = {
						let __w: #wire_ty = gluon_ipc::Convertable::read(gluon_data)?;
						#conv
					};
				}
			} else {
				quote! { let #fname = gluon_ipc::Convertable::read(gluon_data)?; }
			}
		});
		let writes_owned = def.fields.iter().map(|f| {
			let fname = format_ident!("{}", f.name.to_case(Case::Snake));
			if type_has_proxy(&f.ty, gen_ctx) {
				let wire_ty = gen_type(&f.ty, gen_ctx);
				let conv = gen_pub_to_wire(&f.ty, quote! { self.#fname }, gen_ctx);
				quote! { { let __w: #wire_ty = #conv; __w.write_owned(gluon_data)?; } }
			} else {
				quote! { self.#fname.write_owned(gluon_data)?; }
			}
		});
		quote! {
			impl gluon_ipc::Convertable for #name {
				fn write(
					&self,
					gluon_data: &mut gluon_ipc::DataBuilder,
				) -> Result<(), gluon_ipc::WriteError> {
					#(#writes)*
					Ok(())
				}

				fn read(gluon_data: &mut gluon_ipc::DataReader) -> Result<Self, gluon_ipc::ReadError> {
					#(#reads)*
					Ok(#name {#(#field_names,)*})
				}

				fn write_owned(self, gluon_data: &mut gluon_ipc::DataBuilder) -> Result<(), gluon_ipc::WriteError> {
					#(#writes_owned)*
					Ok(())
				}
			}
		}
	};
	quote! {
		#[doc = #doc]
		#[derive(Debug, #(#derives),*)]
		#serde_derives
		pub struct #name {
			#(#fields)*
		}

		#gluon_trait_impl
	}
}

pub fn gen_enum(def: &EnumDef, gen_ctx: &GenCtx) -> proc_macro2::TokenStream {
	let variants = def.variants.iter().map(|variant| {
		let fields = variant
			.fields
			.iter()
			.map(|f| gen_field_enum(f, gen_ctx, type_is_recursive(&f.ty, &def.name, gen_ctx)));
		let name = format_ident!("{}", variant.name.to_case(Case::Pascal));
		let doc_comment = variant.doc.as_ref().map(|str| quote! {#[doc = #str]});
		if !variant.fields.is_empty() {
			quote! {
				#doc_comment
				#name {
					#(#fields)*
				}
			}
		} else {
			quote! {
				#doc_comment
				#name
			}
		}
	});
	let name = def.name.to_case(Case::Pascal);
	let derives = derives_to_tokens(enum_supported_derives(def, gen_ctx));
	let serde_derives = derives_to_serde_tokens(enum_supported_derives(def, gen_ctx));
	let enum_name = format_ident!("{}", name);
	let doc = &def.doc;
	let gluon_trait_impl = {
		let write_variants = def.variants.iter().enumerate().map(|(i, variant)| {
			let field_names = variant
				.fields
				.iter()
				.map(|v| format_ident!("{}", v.name.to_case(Case::Snake)))
				.collect::<Vec<_>>();
			let name = format_ident!("{}", variant.name.to_case(Case::Pascal));
			let i = i as u16;
			if field_names.is_empty() {
				quote! { #enum_name::#name => { gluon_data.write_u16(#i)?; }, }
			} else {
				let field_writes = variant.fields.iter().map(|f| {
					let fname = format_ident!("{}", f.name.to_case(Case::Snake));
					if type_has_proxy(&f.ty, gen_ctx) {
						let wire_ty = gen_type(&f.ty, gen_ctx);
						let conv = gen_pub_to_wire(&f.ty, quote! { #fname.clone() }, gen_ctx);
						quote! { { let __w: #wire_ty = #conv; __w.write_owned(gluon_data)?; } }
					} else {
						quote! { #fname.write(gluon_data)?; }
					}
				});
				quote! {
					#enum_name::#name { #(#field_names),* } => {
						gluon_data.write_u16(#i)?;
						#(#field_writes)*
					},
				}
			}
		});
		let write_owned_variants = def.variants.iter().enumerate().map(|(i, variant)| {
			let field_names = variant
				.fields
				.iter()
				.map(|v| format_ident!("{}", v.name.to_case(Case::Snake)))
				.collect::<Vec<_>>();
			let name = format_ident!("{}", variant.name.to_case(Case::Pascal));
			let i = i as u16;
			if field_names.is_empty() {
				quote! { #enum_name::#name => { gluon_data.write_u16(#i)?; }, }
			} else {
				let field_writes_owned = variant.fields.iter().map(|f| {
					let fname = format_ident!("{}", f.name.to_case(Case::Snake));
					if type_has_proxy(&f.ty, gen_ctx) {
						let wire_ty = gen_type(&f.ty, gen_ctx);
						let conv = gen_pub_to_wire(&f.ty, quote! { #fname }, gen_ctx);
						quote! { { let __w: #wire_ty = #conv; __w.write_owned(gluon_data)?; } }
					} else {
						quote! { #fname.write_owned(gluon_data)?; }
					}
				});
				quote! {
					#enum_name::#name { #(#field_names),* } => {
						gluon_data.write_u16(#i)?;
						#(#field_writes_owned)*
					},
				}
			}
		});
		let read_variants = def.variants.iter().enumerate().map(|(i, variant)| {
			let field_names = variant
				.fields
				.iter()
				.map(|v| format_ident!("{}", v.name.to_case(Case::Snake)))
				.collect::<Vec<_>>();
			let name = format_ident!("{}", variant.name.to_case(Case::Pascal));
			let i = i as u16;
			if variant.fields.is_empty() {
				quote! { #i => { #enum_name::#name }, }
			} else {
				let field_reads = variant.fields.iter().map(|f| {
					let fname = format_ident!("{}", f.name.to_case(Case::Snake));
					if type_has_proxy(&f.ty, gen_ctx) {
						let wire_ty = gen_type(&f.ty, gen_ctx);
						let pub_ty = gen_public_type(&f.ty, gen_ctx);
						let conv = gen_wire_to_pub(&f.ty, quote! { __w }, gen_ctx);
						quote! {
							let #fname: #pub_ty = {
								let __w: #wire_ty = gluon_ipc::Convertable::read(gluon_data)?;
								#conv
							};
						}
					} else {
						quote! { let #fname = gluon_ipc::Convertable::read(gluon_data)?; }
					}
				});
				quote! {
					#i => {
						#(#field_reads)*
						#enum_name::#name { #(#field_names,)* }
					},
				}
			}
		});
		quote! {
			impl gluon_ipc::Convertable for #enum_name {
				fn write(
					&self,
					gluon_data: &mut gluon_ipc::DataBuilder,
				) -> Result<(), gluon_ipc::WriteError> {
					match self {
						#(#write_variants)*
					};
					Ok(())
				}

				fn read(gluon_data: &mut gluon_ipc::DataReader) -> Result<Self, gluon_ipc::ReadError> {
					Ok(match gluon_data.read_u16()? {
						#(#read_variants)*
						v => return Err(gluon_ipc::ReadError::UnknownEnumVariant(v)),
					})
				}

				fn write_owned(self, gluon_data: &mut gluon_ipc::DataBuilder) -> Result<(), gluon_ipc::WriteError> {
					match self {
						#(#write_owned_variants)*
					};
					Ok(())
				}
			}
		}
	};
	quote! {
		#[doc = #doc]
		#[derive(Debug, #(#derives),*)]
		#serde_derives
		pub enum #enum_name {
			#(#variants),*
		}

		#gluon_trait_impl
	}
}

pub fn gen_field_enum(def: &Field, gen_ctx: &GenCtx, boxed: bool) -> proc_macro2::TokenStream {
	let type_def = gen_public_type(&def.ty, gen_ctx);
	let type_def = if boxed {
		quote! { Box<#type_def> }
	} else {
		type_def
	};
	let name = format_ident!("{}", def.name.to_case(Case::Snake));
	let doc_comment = def.doc.as_ref().map(|str| quote! {#[doc = #str]});
	quote! {
		#doc_comment
		#name: #type_def,
	}
}
pub fn gen_field_struct(def: &Field, gen_ctx: &GenCtx, boxed: bool) -> proc_macro2::TokenStream {
	let type_def = gen_public_type(&def.ty, gen_ctx);
	let type_def = if boxed {
		quote! { Box<#type_def> }
	} else {
		type_def
	};
	let name = format_ident!("{}", def.name.to_case(Case::Snake));
	let doc_comment = def.doc.as_ref().map(|str| quote! {#[doc = #str]});
	quote! {
		#doc_comment
		pub #name: #type_def,
	}
}

pub fn gen_custom_type(custom: &CustomType, gen_ctx: &GenCtx) -> proc_macro2::TokenStream {
	match custom {
		CustomType::Named(name) => {
			if find_proxy_by_name(name, gen_ctx).is_some() {
				let name = format_ident!("{}", name.to_case(Case::Pascal));
				quote! {proxied::#name}
			} else {
				let name = format_ident!("{}", name.to_case(Case::Pascal));
				quote! {#name}
			}
		}
		CustomType::Qualified(namespace, type_name) => {
			let import = gen_ctx
				.curr_protocol
				.imports
				.iter()
				.find(|v| &v.alias == namespace)
				.expect("unknown namespace used in qualified type");
			let name = format_ident!("{}", type_name.to_case(Case::Pascal));
			// Check local protocols first (sibling modules in the same output file)
			let (rust_mod, proxied) = gen_ctx
				.other_local_protocols
				.iter()
				.find(|v| v.name == import.name)
				.map(|v| {
					(
						v.rust_module.clone(),
						find_proxy_for_qualified(namespace, type_name, gen_ctx).is_some(),
					)
				})
				.or_else(|| {
					gen_ctx
						.external_protocols
						.iter()
						.find(|v| v.protocol_name == import.name)
						.map(|v| {
							(
								v.rust_module.to_string(),
								find_proxy_for_qualified(namespace, type_name, gen_ctx).is_some(),
							)
						})
				})
				.expect("failed to resolve namespace for qualified type");
			let namespace_path = rust_mod.split("::").map(|v| format_ident!("{}", v));
			if proxied {
				quote! {#(#namespace_path)::*::proxied::#name}
			} else {
				quote! {#(#namespace_path)::*::#name}
			}
		}
	}
}

pub fn gen_type(def: &Type, gen_ctx: &GenCtx) -> proc_macro2::TokenStream {
	match def {
		Type::Bool => quote! {bool},
		Type::U8 => quote! {u8},
		Type::U16 => quote! {u16},
		Type::U32 => quote! {u32},
		Type::U64 => quote! {u64},
		Type::I8 => quote! {i8},
		Type::I16 => quote! {i16},
		Type::I32 => quote! {i32},
		Type::I64 => quote! {i64},
		Type::F32 => quote! {f32},
		Type::F64 => quote! {i64},
		Type::String => quote! {String},
		Type::Fd => quote! {std::os::fd::OwnedFd},
		Type::Ref(ref_type) => match ref_type {
			Some(custom) => gen_custom_type(custom, gen_ctx),
			None => quote! {gluon_ipc::Ref},
		},
		Type::Custom(custom) => gen_custom_type(custom, gen_ctx),
		Type::Array(type_def, len) => {
			let type_def = gen_type(type_def, gen_ctx);
			quote! {[#type_def; #len]}
		}
		Type::Vec(type_def) => {
			let type_def = gen_type(type_def, gen_ctx);
			quote! {Vec<#type_def>}
		}
		Type::Set(type_def) => {
			let type_def = gen_type(type_def, gen_ctx);
			quote! {std::collections::HashSet<#type_def>}
		}
		Type::Map(key, value) => {
			let key = gen_type(key, gen_ctx);
			let value = gen_type(value, gen_ctx);
			quote! {std::collections::HashMap<#key,#value>}
		}
		Type::Option(type_def) => {
			let type_def = gen_type(type_def, gen_ctx);
			quote! {Option<#type_def>}
		}
		Type::Result(ok, err) => {
			let ok = gen_type(ok, gen_ctx);
			let err = gen_type(err, gen_ctx);
			quote! {Result<#ok, #err>}
		}
	}
}

fn struct_supported_derives(def: &StructDef, gen_ctx: &GenCtx) -> Derives {
	let mut visiting = HashSet::new();
	def.fields
		.iter()
		.map(|f| supported_derives_inner(&f.ty, gen_ctx, &mut visiting))
		.fold(gen_ctx.requested_derives, |acc, d| acc & d)
}

fn enum_supported_derives(def: &EnumDef, gen_ctx: &GenCtx) -> Derives {
	let mut visiting = HashSet::new();
	// Enums can't use #[derive(Default)] without a #[default] attribute on a variant,
	// which we don't generate, so always exclude it.
	let requested = gen_ctx.requested_derives - Derives::DEFAULT;
	def.variants
		.iter()
		.flat_map(|v| v.fields.iter())
		.map(|f| supported_derives_inner(&f.ty, gen_ctx, &mut visiting))
		.fold(requested, |acc, d| acc & d)
}

/// Returns which of the requested derives this type can support.
pub fn supported_derives(def: &Type, gen_ctx: &GenCtx) -> Derives {
	supported_derives_inner(def, gen_ctx, &mut HashSet::new())
}

fn supported_derives_inner(
	def: &Type,
	gen_ctx: &GenCtx,
	visiting: &mut HashSet<String>,
) -> Derives {
	let requested = gen_ctx.requested_derives;
	match def {
		Type::Bool
		| Type::U8
		| Type::U16
		| Type::U32
		| Type::U64
		| Type::I8
		| Type::I16
		| Type::I32
		| Type::I64 => requested & Derives::INTEGERS,
		Type::F32 | Type::F64 => requested & Derives::FLOATS,
		Type::String => {
			requested
				& (Derives::CLONE
					| Derives::HASH | Derives::PARTIAL_EQ
					| Derives::EQ | Derives::PARTIAL_ORD
					| Derives::ORD | Derives::DEFAULT
					| Derives::SERDE)
		}
		// OwnedFd doesn't implement any derivable traits (other than Debug)
		Type::Fd => Derives::empty(),
		Type::Ref(_) => {
			requested & (Derives::CLONE | Derives::PARTIAL_EQ | Derives::EQ | Derives::HASH)
		}
		Type::Custom(custom) => custom_type_derives_inner(custom, gen_ctx, visiting),
		Type::Array(v, _) => supported_derives_inner(v, gen_ctx, visiting),
		Type::Vec(v) => supported_derives_inner(v, gen_ctx, visiting) - Derives::COPY,
		Type::Set(v) => supported_derives_inner(v, gen_ctx, visiting) - Derives::COPY,
		Type::Option(v) => supported_derives_inner(v, gen_ctx, visiting),
		Type::Result(ok, err) => {
			supported_derives_inner(ok, gen_ctx, visiting)
				& supported_derives_inner(err, gen_ctx, visiting)
		}
		// TODO: figure out correct semantics
		Type::Map(_, _) => Derives::empty(),
	}
}

fn custom_type_derives_inner(
	custom: &CustomType,
	gen_ctx: &GenCtx,
	visiting: &mut HashSet<String>,
) -> Derives {
	match custom {
		CustomType::Named(name) => {
			if let Some(proxy) = find_proxy_by_name(name, gen_ctx) {
				return gen_ctx.requested_derives & proxy.derives;
			}
			derives_from_protocol_inner(name, gen_ctx, visiting)
		}
		CustomType::Qualified(namespace, type_name) => {
			if let Some(proxy) = find_proxy_for_qualified(namespace, type_name, gen_ctx) {
				return gen_ctx.requested_derives & proxy.derives;
			}
			let import = gen_ctx
				.curr_protocol
				.imports
				.iter()
				.find(|v| &v.alias == namespace)
				.expect("unknown namespace used in qualified type");
			if let Some(v) = gen_ctx
				.other_local_protocols
				.iter()
				.find(|v| v.name == import.name)
			{
				return derives_from_protocol_inner(
					type_name,
					&GenCtx {
						curr_protocol: v,
						..*gen_ctx
					},
					visiting,
				);
			}

			let proto = gen_ctx
				.external_protocols
				.iter()
				.find(|v| v.protocol_name == import.name)
				.unwrap_or_else(|| panic!("unknown import: {namespace}"));
			proto
				.types
				.iter()
				.find(|v| v.name == type_name)
				.unwrap_or_else(|| panic!("unknown type: {type_name}"))
				.supported_derives
		}
	}
}

fn derives_from_protocol_inner(
	type_name: &str,
	gen_ctx: &GenCtx,
	visiting: &mut HashSet<String>,
) -> Derives {
	// Cycle guard: if already computing this type's derives, assume all requested
	// derives are supported so other fields in the cycle can restrict further.
	if !visiting.insert(type_name.to_string()) {
		return gen_ctx.requested_derives;
	}
	let result = gen_ctx
		.curr_protocol
		.enums
		.iter()
		.find(|(n, _)| n == type_name)
		.map(|(_, v)| {
			v.variants
				.iter()
				.flat_map(|v| v.fields.iter())
				.map(|v| supported_derives_inner(&v.ty, gen_ctx, visiting))
				.reduce(|a, b| a & b)
				.unwrap_or(gen_ctx.requested_derives)
		})
		.or_else(|| {
			gen_ctx
				.curr_protocol
				.structs
				.iter()
				.find(|(n, _)| n == type_name)
				.map(|(_, v)| {
					v.fields
						.iter()
						.map(|v| supported_derives_inner(&v.ty, gen_ctx, visiting))
						.reduce(|a, b| a & b)
						.unwrap_or(gen_ctx.requested_derives)
				})
		})
		.or_else(|| {
			gen_ctx
				.curr_protocol
				.interfaces
				.iter()
				.find(|(name, _)| name == type_name)
				.map(|_| gen_ctx.requested_derives & Derives::CLONE)
		})
		// for types with no fields, they support all requested derives
		.unwrap_or_else(|| panic!("unknown type: {type_name}"));
	visiting.remove(type_name);
	result
}

fn derives_to_tokens(derives: Derives) -> Vec<proc_macro2::Ident> {
	let mut out = Vec::new();
	if derives.contains(Derives::COPY) {
		out.push(format_ident!("Copy"));
	}
	if derives.contains(Derives::CLONE) {
		out.push(format_ident!("Clone"));
	}
	if derives.contains(Derives::HASH) {
		out.push(format_ident!("Hash"));
	}
	if derives.contains(Derives::PARTIAL_EQ) {
		out.push(format_ident!("PartialEq"));
	}
	if derives.contains(Derives::EQ) {
		out.push(format_ident!("Eq"));
	}
	if derives.contains(Derives::PARTIAL_ORD) {
		out.push(format_ident!("PartialOrd"));
	}
	if derives.contains(Derives::ORD) {
		out.push(format_ident!("Ord"));
	}
	if derives.contains(Derives::DEFAULT) {
		out.push(format_ident!("Default"));
	}
	out
}

fn derives_to_serde_tokens(derives: Derives) -> proc_macro2::TokenStream {
	if derives.intersects(Derives::SERDE) {
		let mut out = Vec::new();
		if derives.contains(Derives::SERDE_SER) {
			out.push(quote! {serde::Serialize});
		}
		if derives.contains(Derives::SERDE_DE) {
			out.push(quote! {serde::Deserialize});
		}
		// TODO: figure out how to make this a compile error instead of a warn
		quote! {
			#[cfg_attr(feature="serde", derive(#(#out),*))]
		}
	} else {
		quote! {}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use gluon_ipc_parser::parse_idl;

	fn make_ctx(protocol: gluon_ipc_parser::Protocol) -> (LocalProtocol, Derives) {
		let local = LocalProtocol {
			module_name: "test".to_string(),
			rust_module: "test".to_string(),
			protocol,
		};
		(local, Derives::empty())
	}

	#[test]
	fn recursive_struct_direct() {
		// Node directly contains Option<Node> — Option is inline so needs Box
		let (local, derives) = make_ctx(
			parse_idl(
				"test",
				r#"
                /// A linked list node
                struct Node {
                    value: u32,
                    next: Option<Node>,
                }
            "#,
			)
			.unwrap(),
		);
		let gen_ctx = GenCtx {
			curr_protocol: &local,
			other_local_protocols: &[],
			external_protocols: &[],
			requested_derives: derives,
			type_proxies: &[],
			tracing: false,
		};
		let def = &local
			.protocol
			.structs
			.iter()
			.find(|(n, _)| n == "Node")
			.unwrap()
			.1;
		let tokens = gen_struct(def, &gen_ctx).to_string();
		assert!(
			tokens.contains("Box"),
			"expected Box for direct recursive struct:\n{tokens}"
		);
	}

	#[test]
	fn recursive_enum_direct() {
		// Tree enum whose Branch variant embeds Tree directly
		let (local, derives) = make_ctx(
			parse_idl(
				"test",
				r#"
                /// A binary tree
                enum Tree {
                    Leaf {
                        value: u32,
                    },
                    Branch {
                        left: Tree,
                        right: Tree,
                    },
                }
            "#,
			)
			.unwrap(),
		);
		let gen_ctx = GenCtx {
			curr_protocol: &local,
			other_local_protocols: &[],
			external_protocols: &[],
			requested_derives: derives,
			type_proxies: &[],
			tracing: false,
		};
		let def = &local
			.protocol
			.enums
			.iter()
			.find(|(n, _)| n == "Tree")
			.unwrap()
			.1;
		let tokens = gen_enum(def, &gen_ctx).to_string();
		assert!(
			tokens.contains("Box"),
			"expected Box for recursive enum:\n{tokens}"
		);
	}

	#[test]
	fn recursive_mutual_struct_enum() {
		// Expr → Node → Expr is a mutual cycle; both should be boxed
		let (local, derives) = make_ctx(
			parse_idl(
				"test",
				r#"
                /// An expression
                enum Expr {
                    Lit {
                        value: u32,
                    },
                    Add {
                        node: Node,
                    },
                }

                /// A binary node
                struct Node {
                    left: Expr,
                    right: Expr,
                }
            "#,
			)
			.unwrap(),
		);
		let gen_ctx = GenCtx {
			curr_protocol: &local,
			other_local_protocols: &[],
			external_protocols: &[],
			requested_derives: derives,
			type_proxies: &[],
			tracing: false,
		};

		let expr_def = &local
			.protocol
			.enums
			.iter()
			.find(|(n, _)| n == "Expr")
			.unwrap()
			.1;
		let node_def = &local
			.protocol
			.structs
			.iter()
			.find(|(n, _)| n == "Node")
			.unwrap()
			.1;

		let expr_tokens = gen_enum(expr_def, &gen_ctx).to_string();
		let node_tokens = gen_struct(node_def, &gen_ctx).to_string();

		assert!(
			expr_tokens.contains("Box"),
			"expected Box in Expr (mutual recursion):\n{expr_tokens}"
		);
		assert!(
			node_tokens.contains("Box"),
			"expected Box in Node (mutual recursion):\n{node_tokens}"
		);
	}

	#[test]
	fn vec_breaks_recursion_no_box() {
		// Vec is heap-allocated, so children: Vec<Node> should NOT trigger boxing
		let (local, derives) = make_ctx(
			parse_idl(
				"test",
				r#"
                /// A tree node with heap-allocated children
                struct TreeNode {
                    value: u32,
                    children: Vec<TreeNode>,
                }
            "#,
			)
			.unwrap(),
		);
		let gen_ctx = GenCtx {
			curr_protocol: &local,
			other_local_protocols: &[],
			external_protocols: &[],
			requested_derives: derives,
			type_proxies: &[],
			tracing: false,
		};
		let def = &local
			.protocol
			.structs
			.iter()
			.find(|(n, _)| n == "TreeNode")
			.unwrap()
			.1;
		let tokens = gen_struct(def, &gen_ctx).to_string();
		assert!(
			!tokens.contains("Box"),
			"did not expect Box for Vec<T> recursion:\n{tokens}"
		);
	}

	#[test]
	fn non_recursive_no_box() {
		let (local, derives) = make_ctx(
			parse_idl(
				"test",
				r#"
                /// A simple point
                struct Point {
                    x: f32,
                    y: f32,
                }
            "#,
			)
			.unwrap(),
		);
		let gen_ctx = GenCtx {
			curr_protocol: &local,
			other_local_protocols: &[],
			external_protocols: &[],
			requested_derives: derives,
			type_proxies: &[],
			tracing: false,
		};
		let def = &local
			.protocol
			.structs
			.iter()
			.find(|(n, _)| n == "Point")
			.unwrap()
			.1;
		let tokens = gen_struct(def, &gen_ctx).to_string();
		assert!(
			!tokens.contains("Box"),
			"did not expect Box for non-recursive struct:\n{tokens}"
		);
	}
}
