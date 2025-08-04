//! The events in Thorium for triggers and other things to act on

use chrono::prelude::*;
use redis::cmd;
use tracing::{event, instrument, Level};
use uuid::Uuid;

use super::keys::EventKeys;
use crate::models::{Event, EventCacheStatus, EventType};
use crate::utils::{ApiError, Shared};
use crate::{conn, deserialize, query, serialize};

/// Save new events to scylla
///
/// # Arguments
///
/// * `event` - The event to save to scylla
/// * `shared` - Shared Thorium objects
#[instrument(name = "db::events::create", skip_all, err(Debug))]
pub async fn create(event: &Event, shared: &Shared) -> Result<(), ApiError> {
    // get this events type
    let kind = EventType::from(&event.data);
    // get the key to the right event queue
    let key = EventKeys::queue(kind, shared);
    // serialize our event for our event stream
    let serialized = serialize!(event);
    // use our current time as a timestamp
    let now = Utc::now().timestamp();
    // build a pipeline to insert this event into
    let mut pipe = redis::pipe();
    // add this event
    pipe.cmd("zadd").arg(key).arg(now).arg(serialized);
    // execute this query
    let _: () = pipe.query_async(conn!(shared)).await?;
    Ok(())
}

/// Clear some events from the in flight event queue
#[instrument(name = "db::events::clear", skip_all, fields(clears = ids.len()), err(Debug))]
pub async fn clear(kind: EventType, ids: &[Uuid], shared: &Shared) -> Result<(), ApiError> {
    // build the key to the in flight map and the in flight queue
    let map_key = EventKeys::in_flight_map(kind, shared);
    let queue_key = EventKeys::in_flight_queue(kind, shared);
    // build a redis pipeline
    let mut pipe = redis::pipe();
    // clear each of these ids
    for id in ids {
        // remove this id from our in flight queue
        pipe.cmd("zrem").arg(&queue_key).arg(id.to_string());
        // remove it from our in flight data map as well
        pipe.cmd("hdel").arg(&map_key).arg(id.to_string());
    }
    // execute this pipeline
    let _: () = pipe.query_async(conn!(shared)).await?;
    Ok(())
}

/// Try to reset popped events in the case of an error or other failure
///
/// If this reset fails then these events will simply be lost.
///
/// # Arguments
///
/// * `key` - The sorted set to restore these popped events too
/// * `pops` - The popped events to restore
/// * `shared` - Shared Thorium objects
#[instrument(name = "db::events::reset_pops", skip_all, fields(pops = pops.len()), err(Debug))]
async fn reset_pops(
    key: &String,
    pops: Vec<(String, f64)>,
    shared: &Shared,
) -> Result<(), ApiError> {
    // build a pipe to reset any immature events
    let mut pipe = redis::pipe();
    // add all of our immature event reset commands to this pipeline
    for (serial, timestamp) in pops {
        pipe.cmd("zadd").arg(key).arg(timestamp).arg(serial);
    }
    // execute this redis pipeline
    let _: () = pipe.query_async(conn!(shared)).await?;
    Ok(())
}

/// Filter and reset any events that are younger then 3 seconds
///
/// We are filtering events younger then 3 seconds to ensure the DB has a chance
/// to reach consistency.
///
/// # Arguments
///
/// * `key` - The sorted set we are filtering immature events from
/// * `now` - A timestamp from before we pulled events
/// * `serialized` - The serialized events to check for maturity
/// * `shared` - Shared Thorium objects
#[instrument(name = "db::events::filter_immature", skip_all, fields(pre_filter = serialized.len()), err(Debug))]
async fn filter_immature(
    key: &String,
    now: DateTime<Utc>,
    serialized: Vec<(String, f64)>,
    shared: &Shared,
) -> Result<(Vec<Event>, Vec<(String, f64)>), ApiError> {
    // convert our datetime to a timestamp 3 seconds in the past
    let now_ts = (now.timestamp() - 3) as f64;
    // keep track of the events to reset and our deserialized events
    let mut resets = Vec::default();
    let mut events = Vec::with_capacity(serialized.len());
    let mut filtered_serial = Vec::with_capacity(serialized.len());
    // find first mature event that we retrieved
    for (serial, timestamp) in serialized.into_iter().rev() {
        // check if this timestamp is mature or not yet
        if timestamp < now_ts {
            // try to deserialize this mature event
            let event: Event = deserialize!(&serial);
            // add our deserialized event
            events.push(event);
            // add our still serialized but filtered info
            filtered_serial.push((serial, timestamp));
        } else {
            // this event is not yet mature so add it to the reste list
            resets.push((serial, timestamp));
        }
    }
    if !resets.is_empty() {
        // log the number of immature events that we found
        event!(Level::INFO, immature = resets.len());
        // try to reset these immature events
        reset_pops(key, resets, shared).await?;
    }
    Ok((events, filtered_serial))
}

/// Get some number of events to evaluate
///
/// # Arguments
///
/// * `kind` - The kind of events to pop
/// * `count` - The number of events to pop at most
/// * `shared` - Shared Thorium objects
#[rustfmt::skip]
#[instrument(name = "db::events::pop", skip(shared), err(Debug))]
pub async fn pop(kind: EventType, count: usize, shared: &Shared) -> Result<Vec<Event>, ApiError> {
    // build the key to the right event queue
    let key = EventKeys::queue(kind, shared);
    // get our current timestamp
    let now = Utc::now();
    // try to pop some events from the right event queue
    let serialized: Vec<(String, f64)> = query!(cmd("zpopmin").arg(&key).arg(count), shared).await?;
    // filter out and reset any events that are not yet mature
    let (events, filtered) = filter_immature(&key, now, serialized, shared).await?;
    // build the key to the in flight map and the in flight queue
    let map_key = EventKeys::in_flight_map(kind, shared);
    let queue_key = EventKeys::in_flight_queue(kind, shared);
    // build a redis pipeline to add these events to the in flight event queue
    let mut pipe = redis::pipe();
    // add all of our events to the in flight event queue
    for ((serial, timestamp), event) in filtered.iter().zip(events.iter()) {
        // save its data in a separate map so its easy to clean up later
        pipe.cmd("hset").arg(&map_key).arg(event.id.to_string()).arg(serial)
            // save this in flight event id to the in flight queue
            .cmd("zadd").arg(&queue_key).arg(timestamp).arg(event.id.to_string());
    }
    // execute this pipeline
    let _: () = pipe.query_async(conn!(shared)).await?;
    Ok(events)
}

/// Reset all events in our in flight event queue
#[instrument(name = "db::events::reset_all", skip(shared), err(Debug))]
pub async fn reset_all(kind: EventType, shared: &Shared) -> Result<(), ApiError> {
    // build the key to the inflight event map/queue
    let map_key = EventKeys::in_flight_map(kind, shared);
    let queue_key = EventKeys::in_flight_queue(kind, shared);
    // track the number of events we are resetting
    let mut total_reset = 0;
    // pop up to 10,000 items in our in flight event queue 1000 at a time
    loop {
        // try to pop some events from the right event queue
        let popped: Vec<(String, f64)> =
            query!(cmd("zpopmin").arg(&queue_key).arg(1000), shared).await?;
        // if we popped no data then we have no more events to reset
        if popped.is_empty() {
            break;
        }
        // build a redis pipeline
        let mut pipe = redis::pipe();
        // get these events data
        for (event_id, _) in &popped {
            // get this events data
            pipe.cmd("hget").arg(&map_key).arg(event_id);
        }
        // try to get all these events data
        // on failure try to reset our popped events so we don't leak them
        // if the reset fails then the events will be leaked
        let serialized: Vec<String> = match pipe.query_async(conn!(shared)).await {
            Ok(serialized) => serialized,
            Err(error) => {
                // log this error
                event!(Level::ERROR, error = error.to_string());
                // try to reset our popped events
                reset_pops(&queue_key, popped, shared).await?;
                // return our original error if our reset didn't also fail
                return Err(ApiError::from(error));
            }
        };
        // get the key to the right event queue to reset our events into
        let key = EventKeys::queue(kind, shared);
        // build a redis pipeline to reset these events
        let mut pipe = redis::pipe();
        //  crawl over the events we retrieved and reset them
        for ((event_id, timestamp), data) in popped.iter().zip(&serialized) {
            // reset this event back into our event queue
            pipe.cmd("zadd").arg(&key).arg(timestamp).arg(data);
            // remove this events data from our in flight data map
            pipe.cmd("hdel").arg(&map_key).arg(event_id);
        }
        // execute this pipeline
        let _: () = pipe.query_async(conn!(shared)).await?;
        // increment the total number of events that we have reset
        total_reset += popped.len();
        // log the number of events we have reset so far
        event!(Level::INFO, total_reset);
    }
    Ok(())
}

/// get the event cache status
///
/// # Arguments
///
/// * `clear` - Whether to clear the event cache statuses or not
/// * `shared` - Shared Thorium objects
#[instrument(name = "db::events::get_cache_status", skip(shared), err(Debug))]
pub async fn get_cache_status(clear: bool, shared: &Shared) -> Result<EventCacheStatus, ApiError> {
    // get our event handler cache key
    let key = EventKeys::cache(shared);
    // build a redis pipeline
    let mut pipe = redis::pipe();
    // get our cache status
    pipe.atomic().cmd("hget").arg(&key).arg("status");
    // if we also need to clear our cache reset flag then add that
    let triggers = if clear {
        // clear our status flag
        pipe.cmd("hset").arg(&key).arg("status").arg(false);
        // send our query and drop our set response
        let (status, _): (bool, bool) = pipe.query_async(conn!(shared)).await?;
        status
    } else {
        // send our query
        let (status,): (bool,) = pipe.query_async(conn!(shared)).await?;
        status
    };
    // build our event cache status object
    let cache_status = EventCacheStatus { triggers };
    Ok(cache_status)
}
