FROM debian:trixie-slim

ARG RUST_CHANNEL
ARG RUST_TARGETS

ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH \
    CARGO_INCREMENTAL=0 \
    CARGO_TARGET_DIR=/target

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        ca-certificates curl gcc libc6-dev pkg-config \
        musl-tools btrfs-progs xfsprogs dosfstools util-linux e2fsprogs \
        fuse3 bindfs \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/sh fetchloom-other

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --no-modify-path --profile minimal \
        --default-toolchain "$RUST_CHANNEL" \
        --component clippy --component rustfmt \
        $(for target in $RUST_TARGETS; do echo --target "$target"; done) \
    && rm /tmp/rustup-init.sh

RUN tr -d - < /proc/sys/kernel/random/uuid > /etc/machine-id

WORKDIR /workspace
