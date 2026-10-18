FROM rust:1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN groupadd --gid 10001 router && useradd --uid 10001 --gid router --no-create-home router && mkdir -p /app/data && chown router:router /app/data
WORKDIR /app
COPY --from=build /build/target/release/jev-router /usr/local/bin/jev-router
COPY config.production.json /app/config.production.json
USER 10001:10001
EXPOSE 8788
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s CMD ["jev-router", "health"]
STOPSIGNAL SIGTERM
CMD ["jev-router","--config","/app/config.production.json","serve"]
