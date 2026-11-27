# hello-python

The hello campaign in Python. It does what `examples/hello-rust` does: take the
controller lease, create and activate a campaign, put one ground group in it
with a route, watch the world until the group is live and its lead reaches the
route's end, then release the lease.

The gRPC code is generated from `proto/` with `grpcio-tools` into `gen/`, which
git ignores. Python, `uv` and the pinned dependencies (`pyproject.toml`,
`uv.lock`) come from `mise.toml`.

## Generate

    mise install
    mise run hello-python-gen

## Run

Start a server, such as the fake sidecar from a release folder:

    ./nanoprobe-fake-sidecar --listen 127.0.0.1:47311

(on Windows, `nanoprobe-fake-sidecar.exe`) or, in this checkout:

    cargo run -p nanoprobe-fake-sidecar -- --listen 127.0.0.1:47311

Then run the hello:

    mise run hello-python -- http://127.0.0.1:47311

The endpoint is the first argument, or `NANOPROBE_ADDR`, or
`http://127.0.0.1:47311`. If the server needs a token, set `NANOPROBE_TOKEN`;
it is sent as `authorization: Bearer <token>`. The exit code is 0 when the
group arrives and 1 on any failure.

## Check

    mise run hello-check

This builds the fake sidecar, starts it on a free loopback port, runs the hello
against it, and fails on a nonzero exit or after `HELLO_CHECK_SECONDS` (default
60).
