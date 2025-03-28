use proc_macro2::{Ident, TokenStream};
use quote::{quote, ToTokens};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, Expr, Lit, Meta, Token, Variant};

/// Args of `#[rpc_route(service = "service_name")]` attribute.
struct RpcEndpointRouteArgs {
    pub service: Ident,
    pub nats_connection: Expr,
}

impl Parse for RpcEndpointRouteArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut service = None;
        let mut nats_connection = None;
        if input.is_empty() {
            return Err(syn::Error::new(
                input.span(),
                "Expected at least one attribute",
            ));
        }
        let punctuated_options = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        for meta in &punctuated_options {
            match meta {
                Meta::NameValue(name_value) => {
                    let ident = name_value.path.get_ident().ok_or(syn::Error::new_spanned(
                        &name_value.path,
                        "expected identifier",
                    ))?;
                    let ident_str = ident.to_string();
                    let Expr::Lit(value) = name_value.to_owned().value else {
                        return Err(syn::Error::new_spanned(
                            &name_value.value,
                            "expected literal",
                        ));
                    };
                    match (ident_str.as_str(), value.lit) {
                        ("service", Lit::Str(service_name)) => {
                            service = Some(Ident::new(&service_name.value(), service_name.span()));
                        }
                        ("nats_connection", Lit::Str(nats_connection_name)) => {
                            nats_connection = Some(syn::parse_str(&nats_connection_name.value())?);
                        }
                        other => {
                            return Err(syn::Error::new_spanned(
                                other.1,
                                "expected name-value attribute",
                            ));
                        }
                    }
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "expected name-value attribute",
                    ));
                }
            }
        }
        let Some(service) = service else {
            return Err(syn::Error::new(
                input.span(),
                "Expected `service` attribute",
            ));
        };
        let Some(nats_connection) = nats_connection else {
            return Err(syn::Error::new(
                input.span(),
                "Expected `nats_connection` attribute",
            ));
        };
        Ok(RpcEndpointRouteArgs {
            service,
            nats_connection,
        })
    }
}

/// Args of `#[rpc_endpoint(request = "request_name")]` attribute.
struct RpcEndpointArgs {
    pub request: Ident,
}

impl Parse for RpcEndpointArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let options = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        let mut request_ident: Option<_> = None;
        for meta in options {
            if let Meta::NameValue(name_value) = meta {
                if name_value.path.is_ident("request") {
                    let Expr::Lit(value) = &name_value.value else {
                        return Err(syn::Error::new_spanned(
                            &name_value.value,
                            "expected literal",
                        ));
                    };
                    let Lit::Str(request) = &value.lit else {
                        return Err(syn::Error::new_spanned(
                            &value.lit,
                            "expected string literal",
                        ));
                    };
                    request_ident = Some(Ident::new(&request.value(), request.span()));
                }
            }
        }
        let Some(request_ident) = request_ident else {
            return Err(syn::Error::new(
                input.span(),
                "Expected `request` attribute",
            ));
        };
        Ok(RpcEndpointArgs {
            request: request_ident,
        })
    }
}

struct RouteTableEnumInfo {
    pub service: Ident,
    pub nats_connection_static: Expr,
    pub routes: Vec<(Variant, RpcEndpointArgs)>,
    pub enum_ident: Ident,
}

impl RouteTableEnumInfo {
    pub fn generate_subject_mappings(&self) -> Vec<TokenStream> {
        let enum_ident = self.enum_ident.clone();
        self.routes
            .iter()
            .map(|(variant, RpcEndpointArgs { request })| {
                let variant_ident = variant.ident.clone();
                quote! {
                    (#request::subject(), #enum_ident::#variant_ident)
                }
            })
            .collect()
    }
    pub fn route_match_arms(&self) -> Vec<TokenStream> {
        let enum_ident = self.enum_ident.clone();
        self.routes
            .iter()
            .map(|(variant, RpcEndpointArgs { request })| {
                let variant_ident = variant.ident.clone();
                quote! {
                    Some(#enum_ident::#variant_ident) => {
                        match #request::parse_from_bytes(payload.as_ref()) {
                            Ok(parsed) => #request::process_request(
                                service.as_ref(),
                                parsed
                            )
                            .await
                            .map(|response| response.to_bytes().unwrap()),
                            Err(err) => {
                                Err(err.into())
                            }
                        }
                    }
                }
            })
            .collect()
    }
    pub fn streaming_endpoints(&self) -> Vec<TokenStream> {
        let service_name = self.service.clone();
        self.routes
            .iter()
            .map(|(_, RpcEndpointArgs { request })| {
                quote! {
                    format!("{}.{}", #service_name::SERVICE_NAME, #request::ENDPOINT_NAME)
                }
            })
            .map(|subject| {
                quote! {
                    service.endpoint(#subject)
                        .await
                        .expect("Failed to create endpoint")
                }
            })
            .collect()
    }
    pub fn expand(self) -> TokenStream {
        let service = self.service.clone();
        let enum_ident = self.enum_ident.clone();
        let variant_names: Vec<_> = self
            .routes
            .iter()
            .map(|(variant, _)| variant.ident.clone())
            .collect();
        let nats_connection_static = self.nats_connection_static.clone();
        let stream_endpoints = self.streaming_endpoints();
        let all_streams = self.generate_subject_mappings();
        let route_match_arms = self.route_match_arms();
        quote! {
            #[async_trait::async_trait]
            impl ame_bus::pool::PooledApp for #service {
                async fn start_with_pool(
                    app: std::sync::Arc<Self>, mut pool: ame_bus::pool::TokioPool
                ) {
                    use ame_bus::{
                        NatsMessage,
                        service_rpc::*,
                        tracing::{error, warn},
                        futures::{self, StreamExt},
                        message::{
                            NatsCoreMessageSendTrait, 
                            StaticSubjectNatsMessage, 
                        }
                    };
                    use std::collections::HashMap;
                    use std::sync::Arc;
                    use async_nats::service::ServiceExt;

                    let service = Self::set_up_service(#nats_connection_static)
                        .await
                        .expect("Failed to set up service");

                    let mut all_streams = futures::stream::select_all(
                        vec![
                            #(#stream_endpoints),*
                        ]
                    );

                    #[derive(Debug)]
                    enum #enum_ident {
                        #(#variant_names),*
                    }

                    let subjects_list: HashMap<String, #enum_ident> = vec![
                        #(#all_streams),*
                    ].into_iter().collect();
                    let subjects_list = Arc::new(subjects_list);

                    async fn process_all_request(
                        service: Arc<#service>,
                        subjects_list: Arc<HashMap<String, #enum_ident>>,
                        this_request_subject: String,
                        payload: Arc<[u8]>,
                        reply: Option<async_nats::subject::Subject>,
                        nats_client: &async_nats::Client,
                    ) {
                        let exe_result = match subjects_list.get(&this_request_subject) {
                            #(#route_match_arms),*,
                            None => {
                                warn!("Unknown subject: {}", this_request_subject);
                                return;
                            }
                        };
                        match exe_result {
                            Ok(response) => {
                                if let Some(reply) = reply {
                                    if let Err(err) = nats_client.publish(reply, response.to_vec().into()).await {
                                        error!("Failed to publish response: {:?}", err);
                                    }
                                }
                            }
                            Err(err) => {
                                error!("Failed to process request: {:?}", err);
                            }
                        }
                    }

                    while let Some(req) = all_streams.next().await {
                        let async_nats::Message {
                            subject,
                            payload,
                            reply,
                            ..
                        } = req.message;
                        let payload = payload.to_vec().into();
                        pool.run(
                            process_all_request(
                                Arc::clone(&app),
                                Arc::clone(&subjects_list),
                                subject.to_string(),
                                payload,
                                reply,
                                #nats_connection_static,
                            )
                        ).await;
                    }
                }
            }
        }
    }
}

pub fn rpc_route_impl(
    args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let RpcEndpointRouteArgs {
        service,
        nats_connection,
    } = syn::parse_macro_input!(args as RpcEndpointRouteArgs);
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    let enum_name = input.ident.clone();

    let variants = match &input.data {
        syn::Data::Enum(data) => &data.variants,
        _ => {
            return syn::Error::new_spanned(input, "RPC Route Macro can only be used with enum")
                .to_compile_error()
                .into()
        }
    };
    let variants: Vec<_> = variants.iter().cloned().collect();
    let mut variants_args: Vec<RpcEndpointArgs> = Vec::new();
    for variant in variants.iter() {
        let args: TokenStream = variant
            .attrs
            .iter()
            .find_map(|arg| {
                let Meta::List(list) = &arg.meta else {
                    return None;
                };
                if list.path.is_ident("rpc_endpoint") {
                    Some(list.tokens.to_token_stream())
                } else {
                    None
                }
            })
            .ok_or_else(|| syn::Error::new_spanned(variant, "Expected `rpc_endpoint` attribute"))
            .unwrap();

        let args = args.into();
        let args = parse_macro_input!(args as RpcEndpointArgs);
        variants_args.push(args);
    }
    let variants_args = variants_args;

    let routes: Vec<(_, _)> = variants.iter().cloned().zip(variants_args).collect();

    let full_info = RouteTableEnumInfo {
        service,
        routes,
        enum_ident: enum_name,
        nats_connection_static: nats_connection,
    };

    full_info.expand().into()
}
