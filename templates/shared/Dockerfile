FROM rust:1-bookworm AS build
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked && cp target/release/__PACKAGE_NAME__ /usr/local/bin/app

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /usr/local/bin/app /usr/local/bin/app
COPY migrations ./migrations
COPY shopify.app*.toml ./
ENV OPENSHOP_ENV=production PORT=3000
EXPOSE 3000

# Web process. Run workers from the same image with `app worker`,
# and apply migrations before each deploy with `app migrate`.
CMD ["app", "start"]
