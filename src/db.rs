//! All the db-related functions

use chrono::{format::StrftimeItems, NaiveDateTime, Timelike};
use sqlx::{Pool, Sqlite};
use tracing::info;

use crate::Booking;

/// sqlite does not have tz-aware types, so we can only get [`NaiveDateTime`] from it.
/// We ALWAYS STORE UTC DATETIMES IN SQLITE.
struct NaiveBooking {
    booking_id: i64,
    title: String,
    resource_id: i64,
    start_time: chrono::NaiveDateTime,
    end_time: chrono::NaiveDateTime,
}
impl NaiveBooking {
    /// Taking a naive booking, interpret all datetimes as UTC datetimes
    fn interpret_as_utc(self) -> crate::Booking {
        Booking {
            booking_id: self.booking_id,
            title: self.title,
            resource_id: self.resource_id,
            start_time: self.start_time.and_utc(),
            end_time: self.end_time.and_utc(),
        }
    }
}

#[derive(Debug)]
pub enum DBError {
    SelectBookings(sqlx::Error),
    InsertBooking(sqlx::Error),
    DeleteBooking(sqlx::Error),
    UpdateBooking(sqlx::Error),
}
impl core::fmt::Display for DBError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::SelectBookings(e) => {
                write!(
                    f,
                    "Unable to select bookings from the DB. Inner Error: {e}."
                )
            }
            Self::InsertBooking(e) => {
                write!(f, "Unable to insert booking into the DB. Inner Error: {e}.")
            }
            Self::UpdateBooking(e) => {
                write!(f, "Unable to update booking in the DB. Inner Error: {e}.")
            }
            Self::DeleteBooking(e) => {
                write!(f, "Unable to delete booking from the DB. Inner Error: {e}.")
            }
        }
    }
}
impl core::error::Error for DBError {}

#[allow(dead_code)]
async fn get_all_bookings(db: &Pool<Sqlite>) -> Result<Vec<Booking>, DBError> {
    Ok(sqlx::query_as!(
        NaiveBooking,
        "SELECT booking_id, title, resource_id, start_time, end_time FROM bookings;"
    )
    .fetch_all(db)
    .await
    .map_err(DBError::SelectBookings)?
    .into_iter()
    .map(NaiveBooking::interpret_as_utc)
    .collect::<Vec<_>>())
}

/// Get all bookings in the db which intersect the interval [start, end]
pub async fn get_bookings_in_timeframe(
    db: &Pool<Sqlite>,
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<Vec<Booking>, DBError> {
    let fmt = StrftimeItems::new("%Y-%m-%dT%H:%M:%S");
    let start_str = start.format_with_items(fmt.clone()).to_string();
    let end_str = end.format_with_items(fmt.clone()).to_string();
    Ok(sqlx::query_as!(
        NaiveBooking,
        "SELECT booking_id, title, resource_id, start_time, end_time FROM bookings \
         WHERE start_time <= ? AND ? <= end_time;",
        end_str,
        start_str,
    )
    .fetch_all(db)
    .await
    .map_err(DBError::SelectBookings)?
    .into_iter()
    .map(NaiveBooking::interpret_as_utc)
    .collect::<Vec<_>>())
}

/// Insert a booking into the DB
pub async fn insert_booking(db: &Pool<Sqlite>, booking: &Booking) -> Result<(), DBError> {
    let fmt = StrftimeItems::new("%Y-%m-%dT%H:%M:%S");
    let start_str = booking
        .start_time
        .format_with_items(fmt.clone())
        .to_string();
    let end_str = booking.end_time.format_with_items(fmt.clone()).to_string();
    sqlx::query!(
        "INSERT INTO bookings (booking_id, title, resource_id, start_time, end_time) VALUES \
        (?, ?, ?, ?, ?);
        ",
        booking.booking_id,
        booking.title,
        booking.resource_id,
        start_str,
        end_str,
    )
    .execute(db)
    .await
    .map(|_| ())
    .map_err(DBError::InsertBooking)
}

pub async fn insert_bookings<'a, I: Iterator<Item = &'a Booking>>(
    db: &Pool<Sqlite>,
    bookings: I,
) -> Result<(), DBError> {
    for b in bookings {
        insert_booking(db, b).await?;
        info!("Inserted new booking: {b:?}");
    }
    Ok(())
}

pub async fn delete_booking(db: &Pool<Sqlite>, booking_id: i64) -> Result<(), DBError> {
    sqlx::query!(
        "DELETE FROM bookings \
        WHERE booking_id = ?;
        ",
        booking_id,
    )
    .execute(db)
    .await
    .map(|_| ())
    .map_err(DBError::DeleteBooking)
}

pub async fn delete_bookings<I: Iterator<Item = i64>>(
    db: &Pool<Sqlite>,
    bookings: I,
) -> Result<(), DBError> {
    for b in bookings {
        delete_booking(db, b).await?;
    }
    Ok(())
}

pub async fn update_booking(db: &Pool<Sqlite>, booking: &Booking) -> Result<(), DBError> {
    let fmt = StrftimeItems::new("%Y-%m-%dT%H:%M:%S");
    let start_time = booking
        .start_time
        .format_with_items(fmt.clone())
        .to_string();
    let end_time = booking.end_time.format_with_items(fmt).to_string();
    sqlx::query!(
        "UPDATE bookings SET title = ?, resource_id = ?, start_time = ?, end_time = ? \
        WHERE booking_id = ?;
        ",
        booking.title,
        booking.resource_id,
        start_time,
        end_time,
        booking.booking_id,
    )
    .execute(db)
    .await
    .map(|_| ())
    .map_err(DBError::UpdateBooking)
}

pub async fn update_bookings<'a, I: Iterator<Item = &'a Booking>>(
    db: &Pool<Sqlite>,
    bookings: I,
) -> Result<(), DBError> {
    for b in bookings {
        update_booking(db, b).await?;
        info!("Updated Booking {}. Is now: {:?}", b.booking_id, b);
    }
    Ok(())
}

/// Delete old bookings from the DB
///
/// This removes all bookings which have ended anytime before `todayT00:00:00`.
/// In other words: bookings that have ended today are kept. This is because the CT Rest-API only
/// allows granularity down to the day. If we removed bookings from earlier today, the same entries
/// would constantly get rewritten and repruned.
pub async fn prune_old_bookings(db: &Pool<Sqlite>) -> Result<u64, DBError> {
    let time = chrono::Utc::now()
        .naive_utc()
        .with_hour(0)
        .expect("zeroeth hour always exstis")
        .with_minute(0)
        .expect("zeroeth minute always exstis")
        .with_second(0)
        .expect("zeroeth second always exstis");
    let fmt = StrftimeItems::new("%Y-%m-%dT%H:%M:%S");
    let time_str = time.format_with_items(fmt).to_string();
    sqlx::query!("DELETE FROM bookings where end_time < ?;", time_str,)
        .execute(db)
        .await
        .map(|x| x.rows_affected())
        .map_err(DBError::DeleteBooking)
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{DateTime, NaiveDate, TimeDelta};
    use sqlx::SqlitePool;

    #[sqlx::test(fixtures("001_good_data"))]
    async fn select_all_bookings(pool: SqlitePool) {
        let bookings = get_all_bookings(&pool).await.unwrap();
        assert_eq!(bookings.len(), 2);
        assert_eq!(
            bookings[0],
            Booking {
                title: "title".to_owned(),
                booking_id: 123,
                resource_id: 10,
                start_time: DateTime::parse_from_rfc3339("2021-03-26T15:30:00+00:00")
                    .unwrap()
                    .into(),
                end_time: DateTime::parse_from_rfc3339("2021-03-26T17:00:00+00:00")
                    .unwrap()
                    .into(),
            }
        );
        assert_eq!(
            bookings[1],
            Booking {
                title: "title".to_owned(),
                booking_id: 125,
                resource_id: 11,
                start_time: DateTime::parse_from_rfc3339("2021-03-28T15:30:00+00:00")
                    .unwrap()
                    .into(),
                end_time: DateTime::parse_from_rfc3339("2021-03-28T17:00:00+00:00")
                    .unwrap()
                    .into(),
            }
        );
    }

    #[sqlx::test(fixtures("001_good_data"))]
    async fn select_bookings_in_timeframe(pool: SqlitePool) {
        let start = NaiveDate::from_ymd_opt(2021, 3, 26)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let end = NaiveDate::from_ymd_opt(2021, 3, 26)
            .unwrap()
            .and_hms_opt(23, 59, 59)
            .unwrap();
        let bookings = get_bookings_in_timeframe(&pool, start, end).await.unwrap();
        assert_eq!(bookings.len(), 1);
        assert_eq!(
            bookings[0],
            Booking {
                title: "title".to_owned(),
                booking_id: 123,
                resource_id: 10,
                start_time: DateTime::parse_from_rfc3339("2021-03-26T15:30:00+00:00")
                    .unwrap()
                    .into(),
                end_time: DateTime::parse_from_rfc3339("2021-03-26T17:00:00+00:00")
                    .unwrap()
                    .into(),
            }
        );
    }

    #[sqlx::test(fixtures("001_good_data"))]
    async fn delete_single_booking(pool: SqlitePool) {
        delete_booking(&pool, 123).await.unwrap();

        let start = NaiveDate::from_ymd_opt(2021, 3, 26)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let end = NaiveDate::from_ymd_opt(2021, 3, 26)
            .unwrap()
            .and_hms_opt(23, 59, 59)
            .unwrap();
        let bookings = get_bookings_in_timeframe(&pool, start, end).await.unwrap();
        assert_eq!(bookings.len(), 0);
    }

    #[sqlx::test(fixtures("001_good_data"))]
    async fn delete_multiple_bookings(pool: SqlitePool) {
        let to_delete = vec![123, 125];
        delete_bookings(&pool, to_delete.into_iter()).await.unwrap();

        let bookings = get_all_bookings(&pool).await.unwrap();
        assert_eq!(bookings.len(), 0);
    }

    #[sqlx::test(fixtures("001_good_data"))]
    async fn test_update_booking(pool: SqlitePool) {
        let new_booking = Booking {
            title: "title".to_owned(),
            booking_id: 123,
            resource_id: 10,
            start_time: DateTime::parse_from_rfc3339("2021-04-26T15:30:00+00:00")
                .unwrap()
                .into(),
            end_time: DateTime::parse_from_rfc3339("2021-04-26T17:00:00+00:00")
                .unwrap()
                .into(),
        };
        update_booking(&pool, &new_booking).await.unwrap();
        let start = NaiveDate::from_ymd_opt(2021, 4, 20)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let end = NaiveDate::from_ymd_opt(2021, 5, 30)
            .unwrap()
            .and_hms_opt(23, 59, 59)
            .unwrap();
        let bookings = get_bookings_in_timeframe(&pool, start, end).await.unwrap();
        assert_eq!(bookings.len(), 1);
        assert_eq!(bookings[0], new_booking);
    }

    #[sqlx::test(fixtures("001_good_data"))]
    async fn test_insert_booking(pool: SqlitePool) {
        let new_booking = Booking {
            title: "title".to_owned(),
            booking_id: 12341234,
            resource_id: 21,
            start_time: DateTime::parse_from_rfc3339("2019-04-26T14:28:00+00:00")
                .unwrap()
                .into(),
            end_time: DateTime::parse_from_rfc3339("2019-04-26T18:00:00+00:00")
                .unwrap()
                .into(),
        };
        insert_booking(&pool, &new_booking).await.unwrap();
        let start = NaiveDate::from_ymd_opt(2019, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let end = NaiveDate::from_ymd_opt(2019, 12, 31)
            .unwrap()
            .and_hms_opt(23, 59, 59)
            .unwrap();
        let bookings = get_bookings_in_timeframe(&pool, start, end).await.unwrap();
        assert_eq!(bookings.len(), 1);
        assert_eq!(bookings[0], new_booking);
    }

    #[sqlx::test(fixtures("002_empty"))]
    fn test_pruning(pool: SqlitePool) {
        // insert booking for today and tomorrow
        let now = chrono::Utc::now().with_nanosecond(0).unwrap();
        let in_an_hour = now + TimeDelta::hours(1);
        let booking_today = Booking {
            title: "title".to_owned(),
            resource_id: 31,
            booking_id: 9999,
            start_time: now,
            end_time: in_an_hour,
        };
        let yesterday = now - TimeDelta::days(1);
        let yesterday_plus_one_hour = yesterday + TimeDelta::hours(1);
        let booking_yesterday = Booking {
            title: "title".to_owned(),
            resource_id: 31,
            booking_id: 8888,
            start_time: yesterday,
            end_time: yesterday_plus_one_hour,
        };
        insert_bookings(&pool, vec![&booking_yesterday, &booking_today].into_iter())
            .await
            .unwrap();
        // prune
        let rows_changed = prune_old_bookings(&pool).await.unwrap();
        assert_eq!(rows_changed, 1);
        // check that only the one from tomorrow survives
        let bookings = get_all_bookings(&pool).await.unwrap();
        assert_eq!(bookings.len(), 1);
        assert_eq!(bookings[0], booking_today);
    }
}
