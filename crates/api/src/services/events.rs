//! `EventsService`: resumable, never conflated (plan 4.5, 4.7).
//!
//! A subscriber names the sequence number it has, and gets every later event
//! of its log in order, then each new one as it is recorded. The history is
//! sent as fast as the subscriber reads it. Once it has caught up, a
//! subscriber that falls `stream_buffer` events behind the live log is
//! ended with `CONSUMER_TOO_SLOW` and subscribes again from its last
//! sequence number.

use std::sync::Arc;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::events_service_server::EventsService;
use nanoprobe_proto::v1alpha1::{
    ErrorReason, Event, EventFilter, ReadEventsRequest, ReadEventsResponse, SubscribeEventsRequest,
    SubscribeEventsResponse,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Code, Request, Response, Status};

use super::{Out, end, push};
use crate::auth::{Access, require};
use crate::convert::invalid;
use crate::events::{EventLog, passes};
use crate::hub::{Hub, Inner};

/// The service.
pub struct Events(pub Arc<Hub>);

/// The most events one `ReadEvents` returns.
const READ_MAX: usize = 1000;

/// How often a history replay checks whether its reader has made room.
const HISTORY_WAIT: std::time::Duration = std::time::Duration::from_millis(5);

/// Events a subscription takes from the log per wake-up.
const BATCH: usize = 256;

/// Which log a filter reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which {
    Server,
    Campaign(u64),
}

fn which(g: &mut Inner, f: &EventFilter) -> Result<Which, Status> {
    if f.server_log {
        if f.campaign_id != 0 {
            return Err(invalid(
                "filter.campaign_id",
                "the server log names campaign 0",
            ));
        }
        return Ok(Which::Server);
    }
    if f.campaign_id == 0 && g.active.is_none() {
        // Between campaigns the Events service serves the server log
        // (spec/errors.md).
        return Ok(Which::Server);
    }
    Ok(Which::Campaign(g.resolve_read(f.campaign_id)?))
}

fn log(g: &Inner, w: Which) -> Option<&EventLog> {
    match w {
        Which::Server => Some(&g.server_log),
        Which::Campaign(id) => g.campaigns.get(&id).map(|s| &s.events),
    }
}

/// Why a log cannot be read on from `after`.
fn refused(l: &EventLog, after: u64) -> Status {
    if after > l.last() {
        invalid(
            "after_sequence",
            format!(
                "sequence {after} is ahead of the log, whose newest is {}; read from 0",
                l.last()
            ),
        )
    } else {
        status(
            Code::OutOfRange,
            ErrorReason::SequenceExpired,
            format!(
                "events after {after} are no longer all kept; the oldest kept is {}",
                l.first()
            ),
            0,
            "after_sequence",
        )
    }
}

fn page(l: &EventLog, after: u64, f: &EventFilter, limit: usize) -> (Vec<Event>, u64) {
    let mut out = Vec::new();
    let mut last = after;
    for e in l.after(after, usize::MAX) {
        if out.len() == limit {
            break;
        }
        last = e.sequence;
        if passes(e, &f.dcs_ids, &f.player_key) {
            out.push(e.clone());
        }
    }
    (out, last)
}

#[tonic::async_trait]
impl EventsService for Events {
    type SubscribeEventsStream = Out<SubscribeEventsResponse>;

    async fn subscribe_events(
        &self,
        req: Request<SubscribeEventsRequest>,
    ) -> Result<Response<Self::SubscribeEventsStream>, Status> {
        require(&req, Access::Observer)?;
        let r = req.into_inner();
        let f = r.filter.unwrap_or_default();
        let (w, mut cursor, follows) = {
            let mut g = self.0.lock();
            let w = which(&mut g, &f)?;
            let l = log(&g, w).expect("resolved log");
            let cursor = if r.live_only {
                l.last()
            } else if r.after_sequence == 0 {
                l.first().saturating_sub(1)
            } else {
                if !l.covers(r.after_sequence) {
                    return Err(refused(l, r.after_sequence));
                }
                r.after_sequence
            };
            // A stream that names campaign 0 is tied to what is active when
            // it starts, a campaign or none, and ends when that changes.
            let follows = (f.campaign_id == 0 && !f.server_log).then_some(g.active);
            (w, cursor, follows)
        };
        let (tx, rx) = mpsc::channel(self.0.cfg.stream_buffer.max(2));
        let hub = Arc::clone(&self.0);
        tokio::spawn(async move {
            let mut tick = hub.tick();
            let mut live = false;
            loop {
                loop {
                    if hub.closing() {
                        end(&tx, super::stopping());
                        return;
                    }
                    let batch = {
                        let g = hub.lock();
                        if let Some(tied) = follows
                            && g.active != tied
                        {
                            Err(status(
                                Code::FailedPrecondition,
                                ErrorReason::CampaignNotActive,
                                "the active campaign changed",
                                0,
                                "",
                            ))
                        } else {
                            match log(&g, w) {
                                None => Err(status(
                                    Code::NotFound,
                                    ErrorReason::CampaignNotFound,
                                    "the campaign is gone",
                                    0,
                                    "",
                                )),
                                Some(l) if !l.covers(cursor) => Err(super::too_slow()),
                                Some(l) => Ok(page(l, cursor, &f, BATCH)),
                            }
                        }
                    };
                    let (events, last) = match batch {
                        Ok(b) => b,
                        Err(e) => {
                            end(&tx, e);
                            return;
                        }
                    };
                    let more = last != cursor;
                    cursor = last;
                    for e in events {
                        let m = SubscribeEventsResponse { event: Some(e) };
                        if live {
                            if !push(&tx, m) {
                                return;
                            }
                        } else {
                            // History: wait for the reader, keeping one
                            // slot free for the status that ends the stream.
                            while tx.capacity() <= 1 {
                                tokio::select! {
                                    () = tokio::time::sleep(HISTORY_WAIT) => {}
                                    () = tx.closed() => return,
                                }
                            }
                            if tx.try_send(Ok(m)).is_err() {
                                return;
                            }
                        }
                    }
                    if !more {
                        live = true;
                        break;
                    }
                }
                tokio::select! {
                    changed = tick.changed() => if changed.is_err() { return },
                    () = tx.closed() => return,
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn read_events(
        &self,
        req: Request<ReadEventsRequest>,
    ) -> Result<Response<ReadEventsResponse>, Status> {
        require(&req, Access::Observer)?;
        let r = req.get_ref();
        let f = r.filter.clone().unwrap_or_default();
        let limit = match r.limit {
            0 => READ_MAX,
            n => usize::try_from(n).unwrap_or(READ_MAX).min(READ_MAX),
        };
        let mut g = self.0.lock();
        let w = which(&mut g, &f)?;
        let l = log(&g, w).expect("resolved log");
        if r.after_sequence != 0 && !l.covers(r.after_sequence) {
            return Err(refused(l, r.after_sequence));
        }
        let after = r.after_sequence.max(l.first().saturating_sub(1));
        let mut events = Vec::new();
        let mut more = false;
        for e in l.after(after, usize::MAX) {
            if !passes(e, &f.dcs_ids, &f.player_key) {
                continue;
            }
            if events.len() == limit {
                more = true;
                break;
            }
            events.push(e.clone());
        }
        Ok(Response::new(ReadEventsResponse { events, more }))
    }
}
