//! `nanoprobe watch`: a stream watcher for the public API.
//!
//! It prints each message of a world watch or an event subscription on one
//! line as it arrives, so a person can see what a client would see. It
//! changes nothing and needs only the `observer` role.

use std::io::Write;

use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    EventFilter, ListWorldRequest, RateClass, SubscribeEventsRequest, WatchWorldRequest,
    WorldFilter,
};
use tonic::Request;
use tonic::transport::Endpoint;

/// The default API address.
pub const DEFAULT_ADDR: &str = "http://127.0.0.1:47311";

/// What to watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// `WorldService.WatchWorld`.
    World,
    /// `EventsService.SubscribeEvents`.
    Events,
}

/// `nanoprobe watch` options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// What to watch.
    pub stream: Stream,
    /// The API's URL; `None` takes `NANOPROBE_ADDR` or [`DEFAULT_ADDR`].
    pub addr: Option<String>,
    /// The campaign, 0 for the active one.
    pub campaign: u64,
    /// The revision or sequence number to start after.
    pub since: Option<u64>,
    /// The token; `None` takes `NANOPROBE_TOKEN`, if set.
    pub token: Option<String>,
}

/// Parse the arguments after `watch`.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut it = args.iter();
    let stream = match it.next().map(String::as_str) {
        Some("world") => Stream::World,
        Some("events") => Stream::Events,
        Some(other) => return Err(format!("watch takes `world` or `events`, not `{other}`")),
        None => return Err("watch needs `world` or `events`".into()),
    };
    let mut o = Options {
        stream,
        addr: None,
        campaign: 0,
        since: None,
        token: None,
    };
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        let number = |name: &str, v: String| {
            v.parse::<u64>()
                .map_err(|_| format!("{name} needs a whole number"))
        };
        match flag {
            "--addr" => o.addr = Some(value("--addr")?),
            "--campaign" => o.campaign = number("--campaign", value("--campaign")?)?,
            "--since" => o.since = Some(number("--since", value("--since")?)?),
            f => return Err(format!("unknown option `{f}` for `watch`")),
        }
    }
    Ok(o)
}

/// The `authorization` value from the token, or from `NANOPROBE_TOKEN`.
fn token(
    given: Option<&str>,
) -> Result<Option<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>, String> {
    match given
        .map(str::to_owned)
        .or_else(|| std::env::var("NANOPROBE_TOKEN").ok())
    {
        Some(t) => format!("Bearer {t}")
            .parse()
            .map(Some)
            .map_err(|_| "the token is not usable as a header value".to_owned()),
        None => Ok(None),
    }
}

async fn watch(o: &Options, out: &mut dyn Write) -> Result<(), String> {
    let auth = token(o.token.as_deref())?;
    let addr = o
        .addr
        .clone()
        .or_else(|| std::env::var("NANOPROBE_ADDR").ok())
        .unwrap_or_else(|| DEFAULT_ADDR.to_owned());
    let channel = Endpoint::from_shared(addr.clone())
        .map_err(|e| format!("{addr}: {e}"))?
        .connect()
        .await
        .map_err(|e| format!("cannot connect to {addr}: {e}"))?;
    fn authorize<T>(
        mut r: Request<T>,
        auth: &Option<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>,
    ) -> Request<T> {
        if let Some(v) = auth {
            r.metadata_mut().insert("authorization", v.clone());
        }
        r
    }
    let failed = |s: tonic::Status| format!("{}: {}", s.code(), s.message());
    let mut line = |text: String| -> Result<(), String> {
        writeln!(out, "{text}")
            .and_then(|()| out.flush())
            .map_err(|e| format!("output: {e}"))
    };
    match o.stream {
        Stream::World => {
            let mut client = WorldServiceClient::new(channel);
            let mut filter = WorldFilter {
                campaign_id: o.campaign,
                ..WorldFilter::default()
            };
            let since = match o.since {
                Some(n) => n,
                None => {
                    let mut list = client
                        .list_world(authorize(
                            Request::new(ListWorldRequest {
                                filter: Some(filter.clone()),
                            }),
                            &auth,
                        ))
                        .await
                        .map_err(failed)?
                        .into_inner();
                    let mut rev = 0;
                    while let Some(m) = list.message().await.map_err(failed)? {
                        rev = m.revision;
                        // The watch continues the listed campaign, not 0.
                        filter.campaign_id = m.campaign_id;
                        line(format!("list {m:?}"))?;
                    }
                    rev
                }
            };
            let mut s = client
                .watch_world(authorize(
                    Request::new(WatchWorldRequest {
                        filter: Some(filter),
                        since_revision: since,
                        rate: RateClass::Normal as i32,
                        min_period: None,
                    }),
                    &auth,
                ))
                .await
                .map_err(failed)?
                .into_inner();
            while let Some(m) = s.message().await.map_err(failed)? {
                line(format!("watch {m:?}"))?;
            }
        }
        Stream::Events => {
            let mut s = EventsServiceClient::new(channel)
                .subscribe_events(authorize(
                    Request::new(SubscribeEventsRequest {
                        filter: Some(EventFilter {
                            campaign_id: o.campaign,
                            ..EventFilter::default()
                        }),
                        after_sequence: o.since.unwrap_or(0),
                        live_only: false,
                    }),
                    &auth,
                ))
                .await
                .map_err(failed)?
                .into_inner();
            while let Some(m) = s.message().await.map_err(failed)? {
                if let Some(e) = m.event {
                    line(format!("event {e:?}"))?;
                }
            }
        }
    }
    Ok(())
}

/// Run `nanoprobe watch` until the stream ends. Returns the exit code.
pub fn run(o: &Options, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = writeln!(err, "nanoprobe: {e}");
            return 1;
        }
    };
    match rt.block_on(watch(o, out)) {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(err, "nanoprobe: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Options, String> {
        parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn watch_parses() {
        assert_eq!(
            p(&[
                "events",
                "--addr",
                "http://h:1",
                "--since=7",
                "--campaign",
                "3"
            ]),
            Ok(Options {
                stream: Stream::Events,
                addr: Some("http://h:1".into()),
                campaign: 3,
                since: Some(7),
                token: None,
            })
        );
        for bad in [
            &[][..],
            &["sky"],
            &["world", "--since"],
            &["world", "--since", "x"],
            &["world", "--bogus"],
        ] {
            assert!(p(bad).is_err(), "{bad:?}");
        }
    }
}
