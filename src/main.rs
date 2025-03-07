use std::str::FromStr;
use std::sync::Arc;

use chrono::Utc;

use tracing::{debug, error, info};
use tracing_subscriber::{filter, fmt::format::FmtSpan};
use tracing_subscriber::{prelude::*, EnvFilter};

mod config;
mod db;
mod pull_from_ct;
mod web;

pub(crate) const BOOKING_DATABASE_NAME: &str = ".bookings.db";

/// A single booking for a room
#[derive(Debug, PartialEq)]
struct Booking {
    /// the ID of the resource for this booking.
    /// NOTE: this is NOT the ID of the booking, but of the resource in CT.
    /// This ID is used for matching ressources against rooms defined in the config.
    resource_id: i64,
    /// The ID of this booking. This is used to update bookings when they are updated in CT.
    booking_id: i64,
    /// Title of the booking in CT
    title: String,
    /// The booking starts at...
    /// ALL DATETIMES ARE UTC.
    start_time: chrono::DateTime<Utc>,
    /// The booking ends at...
    end_time: chrono::DateTime<Utc>,
}

enum InShutdown {
    Yes,
    No,
}

async fn signal_handler(
    mut watcher: tokio::sync::watch::Receiver<InShutdown>,
    shutdown_tx: tokio::sync::watch::Sender<InShutdown>,
) -> Result<(), std::io::Error> {
    let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        Ok(x) => x,
        Err(e) => {
            error!("Failed to install SIGTERM listener: {e} Aborting.");
            shutdown_tx.send_replace(InShutdown::Yes);
            return Err(e);
        }
    };
    let mut sighup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
        Ok(x) => x,
        Err(e) => {
            error!("Failed to install SIGHUP listener: {e} Aborting.");
            shutdown_tx.send_replace(InShutdown::Yes);
            return Err(e);
        }
    };
    let mut sigint = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
    {
        Ok(x) => x,
        Err(e) => {
            error!("Failed to install SIGINT listener: {e} Aborting.");
            shutdown_tx.send_replace(InShutdown::Yes);
            return Err(e);
        }
    };
    // wait for a shutdown signal
    tokio::select! {
        // shutdown the signal handler when some other process signals a shutdown
        _ = watcher.changed() => {}
        _ = sigterm.recv() => {
            info!("Got SIGTERM. Shuting down.");
            shutdown_tx.send_replace(InShutdown::Yes);
        }
        _ = sighup.recv() => {
            info!("Got SIGHUP. Shuting down.");
            shutdown_tx.send_replace(InShutdown::Yes);
        }
        _ = sigint.recv() => {
            info!("Got SIGINT. Shuting down.");
            shutdown_tx.send_replace(InShutdown::Yes);
        }
        x = tokio::signal::ctrl_c() =>  {
            match x {
                Ok(()) => {
                    info!("Received Ctrl-c. Shutting down.");
                    shutdown_tx.send_replace(InShutdown::Yes);
                }
                Err(err) => {
                    error!("Unable to listen for shutdown signal: {}", err);
                    // we also shut down in case of error
                    shutdown_tx.send_replace(InShutdown::Yes);
                }
            }
        }
    };

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    let config = Arc::new(config::Config::create().await?);
    // Setup tracing

    let my_crate_filter = EnvFilter::new("room_overview");
    let level_filter = filter::LevelFilter::from_str(&config.log_level)?;
    let subscriber = tracing_subscriber::registry().with(my_crate_filter).with(
        tracing_subscriber::fmt::layer()
            .compact()
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .with_line_number(true)
            .with_filter(level_filter),
    );
    tracing::subscriber::set_global_default(subscriber).expect("static tracing config");
    debug!("Tracing enabled");

    // migrate the database
    sqlx::migrate!().run(&config.db).await?;

    // cancellation channel
    let (tx, rx) = tokio::sync::watch::channel(InShutdown::No);

    // start the data-gatherer
    let gatherer_handle = tokio::spawn(pull_from_ct::keep_db_up_to_date(config.clone(), rx));

    // start the Signal handler
    let signal_handle = tokio::spawn(signal_handler(tx.subscribe(), tx.clone()));

    // start the web server
    let web_server = web::run_web_server(config.clone(), tx.subscribe(), tx.clone());

    // Join both tasks
    let (gather_res, signal_res, web_res) =
        tokio::join!(gatherer_handle, signal_handle, web_server,);
    gather_res?;
    signal_res??;
    web_res?;

    Ok(())
}
