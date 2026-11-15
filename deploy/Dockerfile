# The sidecar's container image: one static Linux program and nothing else.
#
# Build the program first, then the image from this folder:
#
#   mise run sidecar-linux
#   cp target/linux/x86_64-unknown-linux-musl/release/nanoprobe-sidecar deploy/
#   docker build -t nanoprobe-sidecar deploy/
#
# The container runs as the user that runs DCS, so it can open the mapped file
# the agent makes. Set it with `user:` in the compose overlay or `--user` on
# `docker run`.

FROM scratch
COPY nanoprobe-sidecar /nanoprobe-sidecar
ENTRYPOINT ["/nanoprobe-sidecar"]
CMD ["run", "--config", "/etc/nanoprobe/sidecar.toml"]
# 0 is healthy; any other verdict, or no answer, is unhealthy (spec/config.md,
# "Health command"). The verdict is the sidecar's view of DCS. Restarting DCS
# on it is the host's choice, not the image's.
HEALTHCHECK --interval=30s --timeout=10s --start-period=30s \
    CMD ["/nanoprobe-sidecar", "health", "--config", "/etc/nanoprobe/sidecar.toml"]
