//! Get data from Churchtools

use std::sync::Arc;

use chrono::Utc;
use itertools::Itertools;
use serde::Deserialize;
use tracing::{debug, info, trace, warn};

use crate::{
    config::Config,
    db::DBError,
    Booking, InShutdown,
};

#[derive(Debug, Deserialize)]
struct CTBookingsResponse {
    data: Vec<BookingsData>,
}
#[derive(Debug, Deserialize)]
struct BookingsData {
    base: BookingsDataBase,
    calculated: BookingsDataCalculated,
}

#[derive(Debug, Deserialize)]
struct BookingsDataBase {
    /// this is the bookings ID
    id: i64,
    resource: ResourceData,
}

#[derive(Debug, Deserialize)]
struct ResourceData {
    /// this is the resources ID
    id: i64,
}

#[derive(Debug, Deserialize)]
struct BookingsDataCalculated {
    #[serde(rename = "startDate")]
    start_date: String,
    #[serde(rename = "endDate")]
    end_date: String,
}

#[derive(Debug)]
pub enum CTApiError {
    GetBookings(reqwest::Error),
    Deserialize,
    Utf8Decode,
    ParseTime(chrono::ParseError),
}
impl std::fmt::Display for CTApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::GetBookings(e) => {
                write!(f, "Cannot get bookings. reqwest Error: {e}")
            }
            Self::Deserialize => {
                write!(f, "Cannot deserialize the response.")
            }
            Self::Utf8Decode=> {
                write!(f, "Cannot decode the message bytes as utf-8.")
            }
            Self::ParseTime(e) => {
                write!(
                    f,
                    "Cannot parse a time contained in CTs response. chrono Error: {e}"
                )
            }
        }
    }
}
impl std::error::Error for CTApiError {}

/// Something went wrong while gathering Information from CT into the DB
#[derive(Debug)]
pub enum GatherError {
    DB(crate::db::DBError),
    CT(CTApiError),
}
impl std::fmt::Display for GatherError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::DB(x) => write!(f, "DBError: {x}"),
            Self::CT(x) => write!(f, "CTApiError: {x}"),
        }
    }
}
impl std::error::Error for GatherError {}
impl From<DBError> for GatherError {
    fn from(value: DBError) -> Self {
        Self::DB(value)
    }
}
impl From<CTApiError> for GatherError {
    fn from(value: CTApiError) -> Self {
        Self::CT(value)
    }
}

async fn get_relevant_bookings(
    config: &Config,
    start_date: chrono::NaiveDate,
    end_date: chrono::NaiveDate,
) -> Result<Vec<Booking>, CTApiError> {
    let mut query_strings = config
        .rooms
        .iter()
        .map(|room_config| room_config.churchtools_id)
        .unique()
        // we now have the resource ids we care about
        // convert them to the query parameters we need
        .map(|id| ("resource_ids[]", format!("{id}")))
        .collect::<Vec<_>>();
    query_strings.push(("from", start_date.to_string()));
    query_strings.push(("to", end_date.to_string()));
    query_strings.push(("status_ids[]", "2".to_owned()));
    // TODO: add login token to request
    let response = match reqwest::Client::new()
        .get(format!("https://{}/api/bookings", config.ct.host))
        .query(&query_strings)
        .header("accept", "application/json")
        .header("Authorization", format!("Login {}", config.ct.login_token))
        .send()
        .await {
            Ok(x) => {
                let text_res = x.text().await;
                match text_res {
                    Ok(text) => {
                        let deser_res: Result<CTBookingsResponse, _> = serde_json::from_str(&text);
                        if let Ok(y) = deser_res {
                            y
                        } else {
                            warn!("There was an error parsing the return value from CT.");
                            warn!("The complete text received was: {text}");
                            return Err(CTApiError::Deserialize);
                        }
                    }
                    Err(e) => {
                        warn!("There was an error reading the response from CT as utf-8: {e}");
                        return Err(CTApiError::Utf8Decode);
                    }
                }
            }
            Err(e) => {
                warn!("There was a problem getting a response from CT");
                return Err(CTApiError::GetBookings(e));
            }
        };
    response
        .data
        .into_iter()
        .map(|x: BookingsData| {
            Ok::<Booking, CTApiError>(Booking {
                booking_id: x.base.id,
                resource_id: x.base.resource.id,
                start_time: chrono::DateTime::parse_from_rfc3339(&x.calculated.start_date)
                    .map_err(CTApiError::ParseTime)?
                    // we get the date from CT with an unknown offset, and need to cast to UTC
                    // (actually, CT seems to always return UTC, but this is not part of a stably documented API)
                    .into(),
                end_time: chrono::DateTime::parse_from_rfc3339(&x.calculated.end_date)
                    .map_err(CTApiError::ParseTime)?
                    .into(),
            })
        })
        .collect::<Result<Vec<_>, _>>()
}

async fn get_bookings_into_db(config: Arc<Config>) -> Result<(), GatherError> {
    let start = Utc::now().naive_utc().into();
    let end = start + chrono::TimeDelta::days(1);
    // get bookings from CT
    let bookings_from_ct = get_relevant_bookings(&config, start, end).await?;
    // get bookings from db
    let bookings_from_db = crate::db::get_bookings_in_timeframe(
        &config.db,
        start.and_time(chrono::NaiveTime::from_hms_opt(0, 0, 0).expect("statically good time")),
        end.and_time(chrono::NaiveTime::from_hms_opt(23, 59, 59).expect("statically good time")),
    )
    .await?;

    // compare the two sources
    // add new bookings
    trace!("in db: {bookings_from_db:?}");
    trace!("in ct: {bookings_from_ct:?}");
    let new_bookings = bookings_from_ct.iter().filter(|b| {
        !bookings_from_db
            .iter()
            .any(|x| x.booking_id == b.booking_id)
    });
    trace!(
        "Adding these bookings: {:?}",
        new_bookings.clone().collect::<Vec<_>>()
    );
    crate::db::insert_bookings(&config.db, new_bookings).await?;

    // remove bookings no longer present in ct
    let deprecated_bookings = bookings_from_db
        .iter()
        .map(|b| b.booking_id)
        .filter(|&id| !bookings_from_ct.iter().any(|x| x.booking_id == id));
    crate::db::delete_bookings(&config.db, deprecated_bookings).await?;

    // Update bookings that have changed times in CT
    let changed_bookings = bookings_from_ct.iter().filter(|b| {
        bookings_from_db
            .iter()
            .any(|x| x.booking_id == b.booking_id && x != *b)
    });
    crate::db::update_bookings(&config.db, changed_bookings).await?;
    Ok(())
}

pub async fn keep_db_up_to_date(
    config: Arc<Config>,
    mut watcher: tokio::sync::watch::Receiver<InShutdown>,
) {
    info!("Starting CT -> DB Sync task");
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
        config.ct.ct_pull_frequency,
    ));
    interval.tick().await;
    loop {
        debug!("Gatherer starting new run.");
        // get new data
        let ct_to_db_res = get_bookings_into_db(config.clone()).await;
        match ct_to_db_res {
            Ok(()) => debug!("Successfully updated db."),
            Err(e) => {
                warn!("Failed to update db from CT. Error encountered: {e}");
            }
        };
        // prune old entries in db
        let db_prune_res = crate::db::prune_old_bookings(&config.db).await;
        match db_prune_res {
            Ok(x) => match x {
                0 => debug!("Successfully pruned db. Removed {x} old bookings."),
                y => info!("Successfully pruned db. Removed {y} old bookings."),
            },
            Err(e) => {
                warn!("Failed to prune db. Error encountered: {e}");
            }
        };
        // stop on cancellation or continue after the next tick
        tokio::select! {
            _ = watcher.changed() => {
                debug!("Shutting down data gatherer now.");
                return;
            }
            _ = interval.tick() => {}
        }
    }
}
