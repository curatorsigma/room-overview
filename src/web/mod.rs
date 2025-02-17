//! The webserver component, creating html views into the cached data.

use askama_axum::Template;
use chrono::Utc;
use uuid::Uuid;

use std::{future::Future, str::FromStr, sync::Arc, time::Duration};

use axum::{
    extract::Host,
    handler::HandlerWithoutStateExt,
    http::{header, HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse, Redirect},
    routing::get,
    Extension, Router,
};
use tracing::{debug, event, Level};

use crate::{config::{Config, RoomConfig}, Booking, InShutdown};

#[derive(Template)]
#[template(path = "500.html")]
struct InternalServerErrorTemplate {
    error_uuid: Uuid,
}

async fn shutdown_signal(handle: axum_server::Handle, mut watcher: tokio::sync::watch::Receiver<InShutdown>) {
    tokio::select! {
        _ = watcher.changed() => {
            debug!("Shutting down web server now.");
            handle.graceful_shutdown(Some(Duration::from_secs(5)));
            return;
        }
    }
}

/// Run the web server
pub async fn run_web_server(
    config: Arc<Config>,
    watcher: tokio::sync::watch::Receiver<InShutdown>,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = Router::new()
        .layer(Extension(config.clone()))
        .route("/style.css", get(css_style))
        .route("/", get(root))
        .fallback(fallback);

    // run it
    let addr = std::net::SocketAddr::from_str(&format!("{}:{}", &config.web.addr, &config.web.tls_port))
        .expect("Should be able to parse socket addr");
    event!(Level::INFO, "Webserver (HTTPS) listening on {}", addr);

    let shutdown_handle = axum_server::Handle::new();
    let shutdown_future = shutdown_signal(shutdown_handle.clone(), watcher.clone());

    // run the redirect service HTTPS -> HTTP on its own port
    tokio::spawn(redirect_http_to_https(config.clone(), shutdown_future));

    // serve the main app on HTTPS
    axum_server::bind_rustls(addr, config.web.rustls_config.clone())
        .handle(shutdown_handle)
        .serve(app.into_make_service())
        .await
        .expect("Should be able to start service");

    Ok(())
}

/// Take an HTTP URI and return the HTTPS equivalent
fn make_https(
    host: String,
    uri: Uri,
    http_port: u16,
    https_port: u16,
) -> Result<Uri, Box<dyn std::error::Error>> {
    let mut parts = uri.into_parts();

    parts.scheme = Some(axum::http::uri::Scheme::HTTPS);

    if parts.path_and_query.is_none() {
        parts.path_and_query = Some("/".parse().expect("Path should be statically save."));
    }

    let https_host = host.replace(&http_port.to_string(), &https_port.to_string());
    parts.authority = Some(https_host.parse()?);

    Ok(Uri::from_parts(parts)?)
}

/// Server redirecting every HTTP request to HTTPS
async fn redirect_http_to_https<F>(config: Arc<Config>, signal: F)
    where F: Future<Output = ()> + Send + 'static
{
    let redir_web_bind_port = config.web.port;
    let redir_web_bind_port_tls = config.web.tls_port;
    let redirect = move |Host(host): Host, uri: Uri| async move {
        match make_https(host, uri, redir_web_bind_port, redir_web_bind_port_tls) {
            Ok(uri) => Ok(Redirect::permanent(&uri.to_string())),
            Err(error) => {
                tracing::warn!(%error, "failed to convert URI to HTTPS");
                Err(StatusCode::BAD_REQUEST)
            }
        }
    };

    let listener = match tokio::net::TcpListener::bind(format!("{}:{}", &config.web.addr, config.web.port)).await {
        Ok(x) => x,
        Err(e) => {
            tracing::error!(
                "Could not bind a TcP socket for the http -> https redirect service: {e}"
            );
            panic!("Unable to start http -> https server. Unrecoverable.");
        }
    };
    tracing::info!(
        "Webserver (HTTP) listening on {}",
        listener
            .local_addr()
            .expect("Local address of bound http -> https should be readable.")
    );
    if let Err(e) = axum::serve(listener, redirect.into_make_service())
        .with_graceful_shutdown(signal)
        .await
    {
        tracing::error!("Could not start the http -> https redirect server: {e}");
        panic!("Unable to start http -> https server. Unrecoverable.");
    };
}

async fn css_style() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::SERVER, "axum".parse().expect("static string"));
    headers.insert(
        header::CONTENT_TYPE,
        "text/css".parse().expect("static string"),
    );
    (headers, include_str!("../../templates/static/style.css"))
}

async fn fallback() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Html(include_str!("../../templates/404.html"))
    )
}

#[derive(Debug)]
struct Event {
    name: String,
    start_time: chrono::DateTime<Utc>,
    room: RoomConfig,
}


#[derive(Debug, Template)]
#[template(path = "landing.html")]
struct LandingTemplate {
    events: Vec<Event>,
}

async fn root() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::SERVER, "axum".parse().expect("static string"));
    // get the current booking states
    // push the templated table
    LandingTemplate { events: vec![] }.into_response()
}

