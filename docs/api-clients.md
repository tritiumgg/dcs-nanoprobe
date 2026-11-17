# Use the API from your language

A campaign controller talks to the sidecar over gRPC, in any language with a gRPC
implementation. nanoprobe publishes no client packages. Generate the client code from
the schema of the release you run, with your language's own Protobuf and gRPC tools.

## The schema

Each GitHub release carries the schema in two forms:

| File | What it holds | Use it with |
|---|---|---|
| `nanoprobe-<version>-schema.tar.gz` | `nanoprobe-<version>-schema/`: `proto/`, `buf.yaml` and `LICENSE` | `protoc` and its plugins, a build-tool plugin, `buf generate` |
| `nanoprobe-<version>-schema.binpb` | A `FileDescriptorSet` of every schema file and the well-known types it imports, with comments | generators that take a descriptor set, `grpcurl -protoset`, dynamic clients |

Both are listed in the release's `SHA256SUMS`. Pin your build to one release's schema
and update it with the server.

The Protobuf package is `nanoprobe.v1alpha1`, in `proto/nanoprobe/v1alpha1/`. Use
`proto/` as the include path. Every file sets `java_package = "nanoprobe.v1alpha1"` and
`java_multiple_files = true`, so Java and Kotlin get one class per message in the
package `nanoprobe.v1alpha1`. While the package is `v1alpha1`, a release may change the
schema in ways that break generated code; its release notes say so.

## Calling the server

The server speaks gRPC over plaintext HTTP/2 on the address `[api] listen` names
(spec/config.md), `127.0.0.1:47311` by default. Every example does the same few
things:

- With a tokens file on the server, send the token in each call's metadata as
  `authorization: Bearer <token>`.
- Call `SessionService.Hello` first, with the schema version you built against. Its
  reply names the schema version the server runs.
- Take the controller lease with `AcquireLease` before any change, and renew it at a
  third of the `ttl` it returns. A lease not renewed in time is lost.
- Give each change an idempotency key that is unique to that change, so a call sent
  again after a network failure is applied once.

The server has gRPC reflection on, so you can try it with no code. Reflection is behind
the same token check as every service: with a tokens file on the server, add the
header to each command.

```sh
buf curl --protocol grpc --http2-prior-knowledge --list-services \
  -H 'authorization: Bearer <token>' http://127.0.0.1:47311
grpcurl -plaintext -H 'authorization: Bearer <token>' 127.0.0.1:47311 list
```

## Examples

Each example is one program that takes the lease, creates and activates a campaign,
puts a ground group in it with a route, and watches the world until the group is live
and its lead reaches the end of the route. The repository's checks run the Rust and
Python examples against the sidecar over a mock of DCS (`lua/tests/sidecar.lua`), and
the Kotlin and Python examples against the fake sidecar.

### Kotlin

[`examples/hello-kotlin`](../examples/hello-kotlin) generates its code at build time
with the Gradle protobuf plugin, the grpc-java and grpc-kotlin generators, and the
Kotlin builtin, which adds DSL builders. Unpack the schema archive and name its
`proto/` folder in `build.gradle.kts`:

```kotlin
sourceSets {
    main { proto { srcDir("nanoprobe-<version>-schema/proto") } }
}
```

The rest of the build file is the example's: the plugin versions, the `grpc-bom`
platform, and the `grpc`, `grpckt` and `kotlin` generators. The BOM is needed:
`grpc-kotlin-stub` depends on an older `grpc-stub` than the generator expects, and
without it the generated code does not compile. A call is a suspend function on a
coroutine stub:

```kotlin
val session = SessionServiceCoroutineStub(channel)
val lease = session.acquireLease(acquireLeaseRequest { clientName = "my-campaign" }).lease
```

### Python

[`examples/hello-python`](../examples/hello-python) generates its code with
`grpcio-tools`:

```sh
python -m grpc_tools.protoc -I nanoprobe-<version>-schema/proto \
  --python_out=gen --pyi_out=gen --grpc_python_out=gen \
  nanoprobe-<version>-schema/proto/nanoprobe/v1alpha1/*.proto
```

The generated modules import each other as `nanoprobe.v1alpha1.<name>_pb2`, so put
`gen/` on `sys.path` and import from `nanoprobe.v1alpha1`. The example calls through
`grpc.aio` and passes the token as `metadata=[("authorization", "Bearer <token>")]`.

### Rust

[`examples/hello-rust`](../examples/hello-rust) uses tonic. In your own crate, generate
the code in `build.rs` from the descriptor set with `tonic-prost-build`, which needs no
`protoc`:

```rust
// build.rs. Build dependencies: prost, prost-types, tonic-prost-build.
use prost::Message as _;

fn main() {
    let path = "nanoprobe-<version>-schema.binpb";
    println!("cargo::rerun-if-changed={path}");
    let bytes = std::fs::read(path).expect("the release's descriptor set");
    let set = prost_types::FileDescriptorSet::decode(bytes.as_slice()).expect("a descriptor set");
    tonic_prost_build::configure()
        .build_server(false)
        .compile_fds(set)
        .expect("code generation");
}
```

```rust
// Dependencies: prost, prost-types, tonic, tonic-prost.
pub mod v1alpha1 {
    tonic::include_proto!("nanoprobe.v1alpha1");
}
```

### Other languages

Any language with a gRPC implementation works the same way: point its Protobuf
generator at `proto/` from the schema archive, or at the descriptor set where the
generator takes one.
