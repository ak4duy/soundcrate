# syntax=docker/dockerfile:1
FROM rust:1.96-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang libclang-dev pkg-config libopus-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
RUN mkdir /playlist-data && chown 10001:10001 /playlist-data
COPY Cargo.toml Cargo.lock ./
COPY src ./src
ARG BUILD_BRANCH=""
ARG BUILD_DATE=""
ARG BUILD_SHA=""
ARG BUILD_REPOSITORY="ak4duy/soundcrate"
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    SOUNDCRATE_BUILD_BRANCH="$BUILD_BRANCH" \
    SOUNDCRATE_BUILD_DATE="$BUILD_DATE" \
    SOUNDCRATE_BUILD_SHA="$BUILD_SHA" \
    SOUNDCRATE_BUILD_REPOSITORY="$BUILD_REPOSITORY" \
    cargo build --locked --release && \
    cp /app/target/release/soundcrate /app/soundcrate && \
    cp -L /usr/lib/*-linux-gnu/libopus.so.0 /app/libopus.so.0

# Match the builder's glibc version; cc includes native runtime libraries and CA certificates.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/libopus.so.0 /usr/lib/libopus.so.0
COPY --from=builder /app/soundcrate /usr/local/bin/soundcrate
COPY --from=builder --chown=10001:10001 /playlist-data /data
USER 10001:10001
ENV MUSIC_DIR=/music
ENV PLAYLIST_DIR=/data
ENTRYPOINT ["/usr/local/bin/soundcrate"]
