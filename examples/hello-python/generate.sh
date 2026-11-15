#!/bin/sh
# Generate the Python gRPC code for the hello campaign from the repository's
# Protobuf, into gen/ (git-ignored). Run it from anywhere.
#
# grpcio-tools ships the well-known types (google/protobuf/*.proto), so
# `proto/` is the only include path.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)

rm -rf "$here/gen"
mkdir -p "$here/gen"
cd "$here"
uv run --frozen python -m grpc_tools.protoc \
  -I "$root/proto" \
  --python_out=gen \
  --pyi_out=gen \
  --grpc_python_out=gen \
  "$root"/proto/nanoprobe/v1alpha1/*.proto
echo "generated $(find gen -name '*_pb2*.py' | wc -l | tr -d ' ') files in $here/gen"
