# Deployment examples

The sidecar beside a DCS dedicated server that runs under Wine in a Docker container.

| File | What it is |
|---|---|
| `Dockerfile` | The sidecar's image: the static Linux program `nanoprobe-sidecar` on an empty base |
| `docker-compose.nanoprobe.yml` | An overlay for your compose file: the sidecar in the DCS container's network namespace, with a shared memory-backed volume for the mapped file |
| `sidecar.toml` | The sidecar's settings for that overlay (spec/config.md) |

## Build the image

```sh
mise install
mise run sidecar-linux
cp target/linux/x86_64-unknown-linux-musl/release/nanoprobe-sidecar deploy/
docker build -t nanoprobe-sidecar:<version> deploy/
```

## Apply the overlay

1. Replace the placeholders in `docker-compose.nanoprobe.yml`: the DCS service's name,
   the user and group that run DCS, and the host path of the DCS profile.
   In `sidecar.toml`, set `[missions] dcs_dir` to the profile's `nanoprobe\missions`
   folder as DCS opens it, and put the template of each theater there as
   `<theater>.miz` (from `templates/`: `Caucasus.miz`, `Afghanistan.miz`). The sidecar
   writes each campaign's shell under `shells/` in that folder.
2. Put `sidecar.toml` in `./nanoprobe/` beside your compose file, and create
   `./nanoprobe/data/` owned by the DCS user.
3. In `<profile>/nanoprobe/agent.toml`, set `map_path = 'Z:\nanoprobe-shm\nanoprobe.map'`.
4. Run `docker compose -f docker-compose.yml -f docker-compose.nanoprobe.yml up -d`.
   The DCS container is created again with the new volume, which restarts DCS.

The overlay's API listener is the DCS container's loopback: a controller in the same
network namespace reaches it as `http://127.0.0.1:47311`. For a controller elsewhere,
`sidecar.toml` and the compose file each show the lines to change: a listener on the
container's address, a tokens file and its mount, the port published on the DCS
service, and the `NANOPROBE_TOKEN` the health check then needs.

`docker compose ps` shows the sidecar's health: the verdict of
`nanoprobe-sidecar health` (spec/config.md). With a `tokens_path`, the health check
needs an observer token in the container's `NANOPROBE_TOKEN`.
