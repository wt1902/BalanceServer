# Rust always runs natively on the build machine and cross-compiles for the
# image platform; only the small runtime stage executes under emulation.
FROM --platform=$BUILDPLATFORM rust:1.90-bookworm AS build
ARG TARGETPLATFORM
# Bundled SQLite is C, so a cross target also needs a C cross compiler and
# bookworm's glibc for that architecture (matching the runtime image).
RUN set -eu; \
    case "$TARGETPLATFORM" in \
      linux/amd64)  target=x86_64-unknown-linux-gnu;     gnu=x86_64-linux-gnu;     arch=amd64 ;; \
      linux/arm64)  target=aarch64-unknown-linux-gnu;    gnu=aarch64-linux-gnu;    arch=arm64 ;; \
      linux/arm/v7) target=armv7-unknown-linux-gnueabihf; gnu=arm-linux-gnueabihf; arch=armhf ;; \
      *) echo "Unsupported platform: $TARGETPLATFORM" >&2; exit 1 ;; \
    esac; \
    echo "export RUST_TARGET=$target" > /cross.env; \
    if [ "$target" != "$(rustc -vV | sed -n 's/^host: //p')" ]; then \
      apt-get update; \
      apt-get install -y --no-install-recommends "gcc-$(echo "$gnu" | tr _ -)" "libc6-dev-$arch-cross"; \
      rm -rf /var/lib/apt/lists/*; \
      rustup target add "$target"; \
      env=$(echo "$target" | tr a-z- A-Z_); \
      echo "export CARGO_TARGET_${env}_LINKER=$gnu-gcc CC_$(echo "$target" | tr - _)=$gnu-gcc AR_$(echo "$target" | tr - _)=$gnu-ar" >> /cross.env; \
    fi
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY internal/httpapi/web ./internal/httpapi/web
RUN . /cross.env \
    && cargo build --locked --release --target "$RUST_TARGET" \
    && cp "target/$RUST_TARGET/release/balance-server" /balance-server

FROM debian:bookworm-slim
# Keep the original Alpine image's numeric identity so existing volumes remain writable.
RUN groupadd --system --gid 101 balance \
    && useradd --system --uid 100 --gid balance --home-dir /nonexistent --no-log-init balance \
    && mkdir /data && chown balance:balance /data
COPY --from=build /balance-server /usr/local/bin/balance-server
USER balance
ENV BALANCE_ADDR=:8080 BALANCE_DB_PATH=/data/balance.db
EXPOSE 8080
VOLUME ["/data"]
ENTRYPOINT ["balance-server"]
