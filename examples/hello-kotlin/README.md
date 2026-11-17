# hello-kotlin

The hello campaign in Kotlin. It does what `examples/hello-rust` does: take the
controller lease, create and activate a campaign, put one ground group in it with a
route, watch the world until the group is live and its lead reaches the route's end,
then release the lease.

The gRPC code is generated from `proto/` at build time by the Gradle protobuf plugin,
with the grpc-java and grpc-kotlin generators, into `build/`, which git ignores. The
JDK and Gradle come from `mise.toml`; Gradle fetches the plugins and libraries that
`build.gradle.kts` pins. Set the Gradle property `nanoprobe.proto` to build against
another schema folder, such as one unpacked from a release's schema archive.

## Run

Start a server, such as the fake sidecar:

    cargo run -p nanoprobe-fake-sidecar -- --listen 127.0.0.1:47311

Then run the hello:

    mise install
    mise run hello-kotlin -- http://127.0.0.1:47311

The endpoint is the first argument, or `NANOPROBE_ADDR`, or `http://127.0.0.1:47311`.
If the server needs a token, set `NANOPROBE_TOKEN`; it is sent as
`authorization: Bearer <token>`. The exit code is 0 when the group arrives and 1 on any
failure.

## Check

    mise run hello-kotlin-check

This builds the hello and the fake sidecar, starts the sidecar on a free loopback port,
runs the hello against it, and fails on a nonzero exit or after `HELLO_CHECK_SECONDS`
(default 60, counted from the sidecar's start).
