FROM rust:1.94-alpine3.23 AS builder

RUN apk add --no-cache build-base cmake perl
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
RUN cargo build --release --locked

FROM alpine:3.23

RUN apk add --no-cache ca-certificates libgcc tini tzdata
COPY --from=builder /build/target/release/backup-repos-checker /usr/local/bin/
COPY --chmod=755 entrypoint.sh /usr/local/bin/entrypoint.sh

WORKDIR /config
ENTRYPOINT ["/sbin/tini", "-g", "--", "/usr/local/bin/entrypoint.sh"]
