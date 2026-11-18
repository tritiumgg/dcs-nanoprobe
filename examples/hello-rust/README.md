# hello-rust

The hello campaign in Rust: the smallest program that runs a campaign through the
nanoprobe API. It takes the controller lease, creates and activates a campaign, puts one
ground group in it with a route, watches the world until the group is live and its lead
reaches the route's end, then releases the lease. `examples/hello-python` and
`examples/hello-kotlin` do the same.

The program is `src/main.rs`; the campaign is `src/lib.rs`. The gRPC code comes from the
workspace's `nanoprobe-proto` crate. A program outside this repository generates its own
from a release's schema ([docs/api-clients.md](../../docs/api-clients.md), "Rust").

## Run

Start a server, such as the fake sidecar:

    cargo run -p nanoprobe-fake-sidecar -- --listen 127.0.0.1:47311

Then run the hello:

    cargo run -p nanoprobe-hello --bin hello-campaign -- http://127.0.0.1:47311

The endpoint is the first argument, or `NANOPROBE_ADDR`, or `http://127.0.0.1:47311`.
If the server needs a token, set `NANOPROBE_TOKEN`; it is sent as
`authorization: Bearer <token>`. The exit code is 0 when the group arrives and 1 on any
failure. The program gives up after 300 seconds.

It prints each step, and ends with:

    hello campaign done: campaign <id> group <id> revision <n> events <n>

## Check

    mise run hello

builds the fake sidecar and the hello, and runs the hello against the fake sidecar on a
free loopback port. The fake sidecar's tests (`crates/fake-sidecar/tests/api.rs`) run
the same campaign.
