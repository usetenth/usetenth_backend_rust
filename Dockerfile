FROM rust:1.90-bookworm AS build
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/usetenth-rust /usr/local/bin/usetenth-rust
# Migrations are not applied by the app: run them (sqlx migrate run) before starting a new version.
EXPOSE 3000
USER nobody
CMD ["usetenth-rust"]
