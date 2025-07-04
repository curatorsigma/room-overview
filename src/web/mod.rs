//! The webserver component, creating html views into the cached data.

use askama_axum::Template;
use chrono::{Local, TimeDelta, Utc};
use uuid::Uuid;

use core::{str::FromStr, time::Duration};
use std::sync::Arc;

use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::get,
    Extension, Router,
};
use tracing::{debug, event, warn, Level};

use crate::{
    config::{Config, RoomConfig},
    db::get_bookings_in_timeframe,
    Booking, InShutdown,
};

#[derive(Template)]
#[template(path = "500.html")]
struct InternalServerErrorTemplate {
    error_uuid: Uuid,
}

async fn shutdown_signal(
    handle: axum_server::Handle,
    mut watcher: tokio::sync::watch::Receiver<InShutdown>,
) {
    tokio::select! {
        _ = watcher.changed() => {
            debug!("Shutting down web server now.");
            handle.graceful_shutdown(Some(Duration::from_secs(5)));
        }
    }
}

/// Run the web server
pub async fn run_web_server(
    config: Arc<Config>,
    watcher: tokio::sync::watch::Receiver<InShutdown>,
    shutdown_tx: tokio::sync::watch::Sender<InShutdown>,
) -> Result<(), Box<dyn core::error::Error>> {
    let app = Router::new()
        .route("/", get(root))
        .route("/all_rooms.ics", get(all_rooms_ics))
        .layer(Extension(config.clone()))
        .route("/style.css", get(css_style))
        .fallback(fallback);

    // run it
    let addr =
        core::net::SocketAddr::from_str(&format!("{}:{}", &config.web.addr, &config.web.tls_port))
            .expect("Should be able to parse socket addr");
    event!(Level::INFO, "Webserver (HTTPS) listening on {}", addr);

    let shutdown_handle = axum_server::Handle::new();
    let shutdown_future = shutdown_signal(shutdown_handle.clone(), watcher.clone());

    let addr =
        std::net::SocketAddr::from_str(&format!("{}:{}", &config.web.addr, &config.web.port))
            .expect("Should be able to parse socket addr");
    // serve the main app on HTTP
    let http_future = axum_server::bind(addr)
        .handle(shutdown_handle.clone())
        .serve(app.clone().into_make_service());

    match &config.web.rustls_config {
        Some(rustls_conf) => {
            let addr_tls = std::net::SocketAddr::from_str(&format!(
                "{}:{}",
                &config.web.addr, &config.web.tls_port
            ))
            .expect("Should be able to parse socket addr_tls");
            // serve the main app on HTTPS
            let https_future = axum_server::bind_rustls(addr_tls, rustls_conf.clone())
                .handle(shutdown_handle.clone())
                .serve(app.into_make_service());
            event!(Level::INFO, "Webserver (HTTP) listening on {}", addr);
            event!(Level::INFO, "Webserver (HTTPS) listening on {}", addr_tls);
            tokio::select! {
                r = http_future => {
                    match r {
                        Ok(()) => {}
                        Err(e) => {
                            tracing::error!("Failure while executing http server: {e}. Shutting down now.");
                            shutdown_tx.send_replace(InShutdown::Yes);
                        }
                    };
                }
                r1 = https_future => {
                    match r1 {
                        Ok(()) => {}
                        Err(e) => {
                            tracing::error!("Failure while executing https server: {e}. Shutting down now.");
                            shutdown_tx.send_replace(InShutdown::Yes);
                        }
                    };
                }
                _ = shutdown_future => {
                }
            };
        }
        None => {
            event!(Level::INFO, "Webserver (HTTP) listening on {}", addr);
            tokio::select! {
                r = http_future => {
                    match r {
                        Ok(()) => {}
                        Err(e) => {
                            tracing::error!("Failure while executing http server: {e}. Shutting down now.");
                            shutdown_tx.send_replace(InShutdown::Yes);
                        }
                    };
                }
                _ = shutdown_future => {
                }
            };
        }
    }

    Ok(())
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
        Html(include_str!("../../templates/404.html")),
    )
}

#[derive(Debug)]
struct Event {
    name: String,
    start_time: chrono::DateTime<Local>,
    end_time: chrono::DateTime<Local>,
    room: RoomConfig,
}
impl Event {
    /// Create this event from a churchtools booking
    fn create_from_booking(value: Booking, config: &Config) -> Option<Self> {
        let room = config
            .rooms
            .iter()
            .find(|r| r.churchtools_id == value.resource_id)?;
        Some(Self {
            name: value.title,
            start_time: value.start_time.into(),
            end_time: value.end_time.into(),
            room: room.clone(),
        })
    }

    /// Is this event currently running?
    fn is_active(&self) -> bool {
        let current_time = Utc::now();
        self.start_time <= current_time && current_time <= self.end_time
    }

    /// human readable start time for this event
    fn hr_start_time(&self) -> String {
        let start_time_in_europe_berlin = self.start_time.with_timezone(&chrono_tz::Europe::Berlin);
        // this must be safe to render without html escaping - it is NOT escaped by the template
        // itself
        format!("{}", start_time_in_europe_berlin.format("%d.%m.<br/>%H:%M"))
    }

    /// start time formatted as ics
    fn ics_start_time(&self) -> String {
        self.start_time
            .with_timezone(&Utc)
            .format("%Y%m%dT%H%M%SZ")
            .to_string()
    }

    /// start time formatted as ics
    fn ics_end_time(&self) -> String {
        self.end_time
            .with_timezone(&Utc)
            .format("%Y%m%dT%H%M%SZ")
            .to_string()
    }

    fn ics_event(self) -> ics::Event<'static> {
        let created_at = Utc::now().naive_utc();
        let mut ics_event = ics::Event::new(
            Uuid::new_v4().to_string(),
            created_at.format("%Y%m%dT%H%M%SZ").to_string(),
        );
        ics_event.push(ics::properties::DtStart::new(self.ics_start_time()));
        ics_event.push(ics::properties::DtEnd::new(self.ics_end_time()));
        ics_event.push(ics::properties::Location::new(self.room.ics_location()));
        ics_event.push(ics::properties::Summary::new(self.name));
        ics_event
    }
}

#[derive(Debug, Template)]
#[template(path = "landing.html")]
struct LandingTemplate {
    events: Vec<Event>,
}

async fn root(Extension(config): Extension<Arc<Config>>) -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::SERVER, "axum".parse().expect("static string"));
    // get the current booking states
    let start = Utc::now().naive_utc();
    let end = start + TimeDelta::minutes(120);
    let bookings = match get_bookings_in_timeframe(&config.db, start, end).await {
        Ok(x) => x,
        Err(e) => {
            let error_uuid = Uuid::new_v4();
            warn!("Sending internal server error because there was a problem getting bookings.");
            warn!("DBError: {e} Error-UUID: {error_uuid}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                InternalServerErrorTemplate { error_uuid },
            )
                .into_response();
        }
    };
    let Some(events) = bookings
        .into_iter()
        .map(|b| Event::create_from_booking(b, &config))
        .collect::<Option<Vec<_>>>()
    else {
        let error_uuid = Uuid::new_v4();
        warn!("Sending internal server error because there was a problem assigning bookings to rooms.");
        warn!("Error-UUID: {error_uuid}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            InternalServerErrorTemplate { error_uuid },
        )
            .into_response();
    };

    // push the templated table
    LandingTemplate { events }.into_response()
}

async fn all_rooms_ics(Extension(config): Extension<Arc<Config>>) -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::SERVER, "axum".parse().expect("static string"));
    // get the current booking states
    let start = Utc::now().naive_utc();
    let end = start + TimeDelta::minutes(120);
    let bookings = match get_bookings_in_timeframe(&config.db, start, end).await {
        Ok(x) => x,
        Err(e) => {
            let error_uuid = Uuid::new_v4();
            warn!("Sending internal server error because there was a problem getting bookings.");
            warn!("DBError: {e} Error-UUID: {error_uuid}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                InternalServerErrorTemplate { error_uuid },
            )
                .into_response();
        }
    };
    let Some(events) = bookings
        .into_iter()
        .map(|b| Event::create_from_booking(b, &config))
        .collect::<Option<Vec<_>>>()
    else {
        let error_uuid = Uuid::new_v4();
        warn!("Sending internal server error because there was a problem assigning bookings to rooms.");
        warn!("Error-UUID: {error_uuid}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            InternalServerErrorTemplate { error_uuid },
        )
            .into_response();
    };

    // Create a new ics string
    let mut calendar = ics::ICalendar::new("2.0", "ics-rs");
    for event in events {
        calendar.add_event(event.ics_event());
    }
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        "content-type",
        "text/calendar;charset=utf-8"
            .parse()
            .expect("static string"),
    );
    (StatusCode::OK, resp_headers, calendar.to_string()).into_response()
}
