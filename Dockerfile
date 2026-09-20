# syntax=docker/dockerfile:1
FROM rust:1.96-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang libclang-dev pkg-config libopus-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo build --locked --release && \
    cp /app/target/release/soundcrate /app/soundcrate

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libopus0 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/soundcrate /usr/local/bin/soundcrate
USER 10001:10001
ENV MUSIC_DIR=/music
ENTRYPOINT ["soundcrate"]
