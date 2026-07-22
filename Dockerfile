FROM rust:1-slim-bookworm AS builder

WORKDIR /src

# Build dependencies on their own layer so source edits don't trigger a full
# dependency rebuild.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release \
    && rm -rf src

COPY src ./src
# Cargo skips rebuilding when only mtime changed, so force it for the real main.
RUN touch src/main.rs && cargo build --release


FROM debian:bookworm-slim

# curl is here purely for the container healthcheck below.
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /home/rustdesk rustdesk \
    && mkdir -p /data && chown rustdesk:rustdesk /data

COPY --from=builder /src/target/release/rustdesk-api /usr/local/bin/rustdesk-api

USER rustdesk
VOLUME ["/data"]
EXPOSE 21114

ENV RDAPI_BIND=0.0.0.0:21114 \
    RDAPI_DB=sqlite:///data/rustdesk-api.db

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:21114/health || exit 1

ENTRYPOINT ["rustdesk-api"]
CMD ["serve"]
