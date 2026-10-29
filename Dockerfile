FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY internal/httpapi/web ./internal/httpapi/web
RUN cargo build --locked --release

FROM debian:bookworm-slim
# Keep the original Alpine image's numeric identity so existing volumes remain writable.
RUN groupadd --system --gid 101 balance \
    && useradd --system --uid 100 --gid balance --home-dir /nonexistent --no-log-init balance \
    && mkdir /data && chown balance:balance /data
COPY --from=build /src/target/release/balance-server /usr/local/bin/balance-server
USER balance
ENV BALANCE_ADDR=:8080 BALANCE_DB_PATH=/data/balance.db
EXPOSE 8080
VOLUME ["/data"]
ENTRYPOINT ["balance-server"]
