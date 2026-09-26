# DropBridge relay — blind encrypted packet forwarder.
# Build from the repo root:
#   docker build -f dropbridge/deploy/relay/Dockerfile -t dropbridge-relay .
FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY dropbridge /src/dropbridge
RUN cargo build --manifest-path dropbridge/Cargo.toml -p dropbridge-relay --release \
 && mkdir /out && cp dropbridge/target/release/dropbridge-relay /out/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -r -u 10001 dropbridge
COPY --from=build /out/dropbridge-relay /usr/local/bin/dropbridge-relay
USER dropbridge
EXPOSE 8080 9090
HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
  CMD curl -fsS http://127.0.0.1:9090/metrics >/dev/null || exit 1
ENTRYPOINT ["/usr/local/bin/dropbridge-relay"]
CMD ["--bind", "0.0.0.0:8080", "--metrics", "0.0.0.0:9090"]
