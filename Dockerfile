# syntax=docker/dockerfile:1.7
FROM rust:1.98-bookworm AS rust-source
ENV PATH="/usr/local/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu/bin:${PATH}"
WORKDIR /src
RUN rustup toolchain install 1.97.1 --profile minimal && \
    rustup component add clippy rustfmt --toolchain 1.97.1

COPY Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml ./
COPY migrations ./migrations
COPY crates ./crates
COPY apps ./apps
COPY tools/textcomb-eval ./tools/textcomb-eval

FROM rust-source AS rust-builder
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release \
      --bin textcomb-api --bin textcomb-worker --bin textcomb-cli && \
    install -Dm755 target/release/textcomb-api /out/textcomb-api && \
    install -Dm755 target/release/textcomb-worker /out/textcomb-worker && \
    install -Dm755 target/release/textcomb-cli /out/textcomb-cli

FROM rust-source AS test
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo fmt --all -- --check && \
    cargo test --locked --workspace --all-targets && \
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings && \
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps

FROM ghcr.io/typst/typst:0.14.2 AS typst

FROM debian:bookworm-slim AS runtime-base
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates dumb-init && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 10001 textcomb && \
    useradd --uid 10001 --gid 10001 --create-home --shell /usr/sbin/nologin textcomb && \
    install -d -o textcomb -g textcomb /var/lib/textcomb
WORKDIR /app
USER textcomb:textcomb
ENTRYPOINT ["dumb-init", "--"]

FROM runtime-base AS api
COPY --from=rust-builder /out/textcomb-api /usr/local/bin/textcomb-api
EXPOSE 8080
CMD ["textcomb-api"]

FROM runtime-base AS worker
USER root
RUN apt-get update && \
    apt-get install -y --no-install-recommends fontconfig fonts-noto-cjk poppler-utils && \
    rm -rf /var/lib/apt/lists/*
COPY --from=typst /bin/typst /usr/local/bin/typst
COPY --from=rust-builder /out/textcomb-worker /usr/local/bin/textcomb-worker
RUN fc-cache -f
USER textcomb:textcomb
CMD ["textcomb-worker"]

FROM worker AS cli
USER root
COPY --from=rust-builder /out/textcomb-cli /usr/local/bin/textcomb-cli
USER textcomb:textcomb
ENTRYPOINT ["dumb-init", "--", "textcomb-cli"]
