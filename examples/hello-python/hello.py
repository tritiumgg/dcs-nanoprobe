"""The hello campaign in Python: the smallest program that runs a campaign
through the nanoprobe API. It does what examples/hello-rust does.

It takes the controller lease, creates and activates a campaign, puts one
ground group in it with a route, and watches the world until the group is
live in DCS and its lead has reached the route's end, renewing its lease at a
third of the lease's time as it goes. Then it gives the lease up.

The server's URL is the first argument, or NANOPROBE_ADDR, or
http://127.0.0.1:47311. A token, if the server needs one, is read from
NANOPROBE_TOKEN. Generate the gRPC code first with generate.sh.
"""

from __future__ import annotations

import asyncio
import math
import os
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Awaitable, Callable, TypeVar
from urllib.parse import urlparse

import grpc

sys.path.insert(0, str(Path(__file__).resolve().parent / "gen"))

from nanoprobe.v1alpha1 import (  # noqa: E402
    campaign_pb2,
    campaign_pb2_grpc,
    commands_pb2,
    commands_pb2_grpc,
    common_pb2,
    events_pb2,
    events_pb2_grpc,
    session_pb2,
    session_pb2_grpc,
    world_pb2,
    world_pb2_grpc,
)

# Where the hello campaign runs: the theater, a start and an end.
THEATER = "Caucasus"
START = (-290_000.0, 680_000.0)
END = (-289_000.0, 680_000.0)
# How close the lead must come to the end.
ARRIVED_M = 50.0
# The unit type of both units, and the country of the group.
UNIT_TYPE = "M-113"
COUNTRY = 2
# Fails after this long.
LIMIT_S = 300.0
# The shortest wait between lease renewals.
MIN_RENEW_S = 0.01
SCHEMA_VERSION = "v1alpha1"
CLIENT_NAME = "hello-python"
DEFAULT_ADDR = "http://127.0.0.1:47311"

T = TypeVar("T")


class HelloError(Exception):
    """A failure the program reports and exits 1 on."""


@dataclass
class Summary:
    """What the run saw."""

    campaign: int = 0
    group: int = 0
    revision: int = 0
    states: list[str] = field(default_factory=list)
    events: int = 0


def short(enum_type: object, value: int, prefix: str) -> str:
    """An enum value's name without its prefix, in title case: Live."""
    name = enum_type.Name(value)  # type: ignore[attr-defined]
    return name.removeprefix(prefix).title()


def key(campaign: str, what: str) -> common_pb2.Idempotency:
    return common_pb2.Idempotency(key=f"hello/{campaign}/{what}")


def at(north: float, east: float) -> common_pb2.Position:
    return common_pb2.Position(theater=THEATER, north=north, east=east, alt=0.0)


async def call(what: str, awaitable: Awaitable[T]) -> T:
    """Await a call, turning a gRPC error into `What: CODE message`."""
    try:
        return await awaitable
    except grpc.aio.AioRpcError as e:
        raise HelloError(f"{what}: {e.code().name} {e.details()}") from None


def target_of(endpoint: str) -> str:
    """The host:port gRPC dials, from a URL such as http://127.0.0.1:47311."""
    if "://" not in endpoint:
        return endpoint
    u = urlparse(endpoint)
    if u.scheme != "http" or not u.hostname:
        raise HelloError(f"endpoint: only http:// URLs are supported: {endpoint}")
    return f"{u.hostname}:{u.port or 80}"


async def run(endpoint: str, token: str | None, limit: float) -> Summary:
    """Run the hello campaign against `endpoint`. Fails after `limit` seconds."""
    channel = grpc.aio.insecure_channel(target_of(endpoint))
    try:
        return await asyncio.wait_for(_run(channel, token), timeout=limit)
    except asyncio.TimeoutError:
        raise HelloError(f"the group did not arrive within {limit:g}s") from None
    finally:
        await channel.close()


async def _run(channel: grpc.aio.Channel, token: str | None) -> Summary:
    md = (("authorization", f"Bearer {token}"),) if token else ()
    session = session_pb2_grpc.SessionServiceStub(channel)
    campaigns = campaign_pb2_grpc.CampaignServiceStub(channel)
    commands = commands_pb2_grpc.CommandsServiceStub(channel)
    world = world_pb2_grpc.WorldServiceStub(channel)
    events = events_pb2_grpc.EventsServiceStub(channel)

    hello = await call(
        "Hello",
        session.Hello(
            session_pb2.HelloRequest(client_name=CLIENT_NAME, schema_version=SCHEMA_VERSION),
            metadata=md,
        ),
    )
    print(
        f"server {hello.server_version} schema {hello.schema_version} "
        f"role {short(session_pb2.Role, hello.role, 'ROLE_')}"
    )

    granted = await call(
        "AcquireLease",
        session.AcquireLease(
            session_pb2.AcquireLeaseRequest(client_name=CLIENT_NAME), metadata=md
        ),
    )
    lease = granted.lease
    if not granted.HasField("ttl") or granted.ttl.seconds < 0 or granted.ttl.nanos < 0:
        raise HelloError("AcquireLease returned no lease time")
    ttl = granted.ttl.seconds + granted.ttl.nanos / 1e9
    renew_every = max(ttl / 3, MIN_RENEW_S)
    print(f"lease epoch {lease.epoch}")

    # A name per run, so a second run on the same server makes its own.
    name = f"hello-{time.time_ns() // 1_000_000}"
    created = await call(
        "CreateCampaign",
        campaigns.CreateCampaign(
            campaign_pb2.CreateCampaignRequest(
                lease=lease,
                name=name,
                theaters=[THEATER],
                first_theater=THEATER,
                activate=True,
                idempotency=key(name, "create"),
            ),
            metadata=md,
        ),
    )
    if not created.HasField("campaign"):
        raise HelloError("CreateCampaign returned no campaign")
    campaign = created.campaign
    print(f"campaign {campaign.id} `{campaign.name}`")

    summary = Summary(campaign=campaign.id)

    async def renew() -> None:
        while True:
            await asyncio.sleep(renew_every)
            await call(
                "RenewLease",
                session.RenewLease(session_pb2.RenewLeaseRequest(lease=lease), metadata=md),
            )

    async def follow_events(stream: grpc.aio.UnaryStreamCall) -> None:
        while True:
            e = await call("SubscribeEvents", stream.read())
            if e is grpc.aio.EOF:
                raise HelloError("the event stream ended")
            ev = e.event
            summary.events += 1
            body = ev.WhichOneof("body")
            if body == "lifecycle":
                print(
                    f"event {ev.sequence}: sim "
                    f"{short(events_pb2.SimState, ev.lifecycle.state, 'SIM_STATE_')} "
                    f"on `{ev.lifecycle.theater}`"
                )
            elif body == "world":
                print(f"event {ev.sequence}: DCS event {ev.world.dcs_id}")
            elif body == "gap":
                print(f"event {ev.sequence}: {ev.gap.lost} events lost")
            else:
                print(f"event {ev.sequence}")

    # Events from the start of the campaign's log.
    stream = events.SubscribeEvents(
        events_pb2.SubscribeEventsRequest(
            filter=events_pb2.EventFilter(campaign_id=campaign.id),
            after_sequence=0,
            live_only=False,
        ),
        metadata=md,
    )

    def unit(label: str, east: float) -> commands_pb2.UnitSpec:
        return commands_pb2.UnitSpec(
            type=UNIT_TYPE, label=label, position=at(START[0], START[1] + east)
        )

    put = await call(
        "PutGroup",
        commands.PutGroup(
            commands_pb2.PutGroupRequest(
                write=session_pb2.Write(
                    lease=lease, idempotency=key(name, "convoy"), campaign_id=campaign.id
                ),
                group_id=0,
                spec=commands_pb2.GroupSpec(
                    label="convoy",
                    country=COUNTRY,
                    category=common_pb2.GROUP_CATEGORY_GROUND,
                    units=[unit("lead", 0.0), unit("second", 20.0)],
                ),
                order=commands_pb2.Order(
                    route=commands_pb2.RouteOrder(
                        waypoints=[
                            commands_pb2.Waypoint(
                                position=at(END[0], END[1]), speed=10.0, on_road=False
                            )
                        ]
                    )
                ),
            ),
            metadata=md,
        ),
    )
    print(f"group {put.group_id} units {list(put.unit_ids)} at revision {put.revision}")
    if not put.unit_ids:
        raise HelloError("PutGroup returned no units")
    lead = put.unit_ids[0]
    summary.group = put.group_id

    # List, then watch from the listed revision.
    wfilter = world_pb2.WorldFilter(campaign_id=campaign.id)
    listed = world.ListWorld(world_pb2.ListWorldRequest(filter=wfilter), metadata=md)
    revision = 0
    while True:
        page = await call("ListWorld", listed.read())
        if page is grpc.aio.EOF:
            break
        revision = page.revision
    watch = world.WatchWorld(
        world_pb2.WatchWorldRequest(
            filter=wfilter, since_revision=revision, rate=world_pb2.RATE_CLASS_FAST
        ),
        metadata=md,
    )

    async def follow_world() -> None:
        live = False
        arrived = False
        while not (live and arrived):
            m = await call("WatchWorld", watch.read())
            if m is grpc.aio.EOF:
                raise HelloError("the watch ended")
            summary.revision = m.revision
            for c in m.changes:
                kind = c.WhichOneof("change")
                if kind == "upsert" and c.upsert.WhichOneof("kind") == "group":
                    g = c.upsert.group
                    if g.id != put.group_id:
                        continue
                    s = short(world_pb2.EntityState, g.state, "ENTITY_STATE_")
                    if not summary.states or summary.states[-1] != s:
                        print(f"group {g.id} is {s} at revision {m.revision}")
                        summary.states.append(s)
                    live = s == "Live"
                elif kind == "positions":
                    b = c.positions
                    for i, u in enumerate(b.unit_id):
                        if u == lead:
                            d = math.hypot(b.north[i] - END[0], b.east[i] - END[1])
                            if d <= ARRIVED_M and not arrived:
                                print(f"lead {lead} reached the end of its route")
                                arrived = True

    tasks = [
        asyncio.ensure_future(renew()),
        asyncio.ensure_future(follow_events(stream)),
        asyncio.ensure_future(follow_world()),
    ]
    try:
        # Return when the world task ends; the other two only end on an error.
        await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
        for t in tasks:
            if t.done():
                t.result()
    finally:
        for t in tasks:
            t.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        for c in (stream, watch):
            c.cancel()

    await call(
        "ReleaseLease",
        session.ReleaseLease(session_pb2.ReleaseLeaseRequest(lease=lease), metadata=md),
    )
    print("done: lease released")
    return summary


async def main() -> int:
    endpoint = (sys.argv[1] if len(sys.argv) > 1 else None) or os.environ.get(
        "NANOPROBE_ADDR", DEFAULT_ADDR
    )
    token = os.environ.get("NANOPROBE_TOKEN")
    try:
        s = await run(endpoint, token, LIMIT_S)
    except HelloError as e:
        print(f"hello-python: {e}", file=sys.stderr)
        return 1
    print(
        f"hello campaign done: campaign {s.campaign} group {s.group} "
        f"revision {s.revision} events {s.events}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
