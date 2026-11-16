FROM rust:1.90-bookworm AS build
WORKDIR /app
# Build the dependencies first so code-only changes rebuild in seconds.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY assets ./assets
COPY migrations ./migrations
COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/usetenth-rust /usr/local/bin/usetenth-rust
# Migrations are not applied by the app: run them (sqlx migrate run) before starting a new version.
# No settings or keys are baked in: everything comes from the environment at run time.
EXPOSE 3000
USER nobody
CMD ["usetenth-rust"]
