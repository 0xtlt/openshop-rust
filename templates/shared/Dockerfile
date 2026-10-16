FROM rust:1-bookworm AS build
# if views
# What builds the admin UI bundle: the WebAssembly target, trunk, and the
# openshop CLI of the tag, branch or rev of openshop that Cargo.toml names.
RUN rustup target add wasm32-unknown-unknown
RUN curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-$(uname -m)-unknown-linux-gnu.tar.gz" \
    | tar -xz -C /usr/local/bin trunk
COPY Cargo.toml /tmp/openshop-cli/Cargo.toml
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/tmp/openshop-cli/target \
    source=$(grep -m1 '^openshop = ' /tmp/openshop-cli/Cargo.toml | grep -oE '(tag|branch|rev) = "[^"]+"' | head -1 | sed -E 's/ = "(.*)"/ \1/'); \
    if [ -z "$source" ]; then echo "Cargo.toml names no tag, branch or rev of openshop" >&2; exit 1; fi; \
    cargo install --locked --target-dir /tmp/openshop-cli/target \
      --git https://github.com/0xtlt/openshop-rust --"${source%% *}" "${source#* }" openshop-cli
# end
WORKDIR /app
COPY . .
# `--locked` as soon as the project has its Cargo.lock.
# if views
# The admin UI bundle into dist/, then the app.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    openshop build $(test -f Cargo.lock && echo --locked) && cp target/release/__PACKAGE_NAME__ /usr/local/bin/app
# else
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release $(test -f Cargo.lock && echo --locked) && cp target/release/__PACKAGE_NAME__ /usr/local/bin/app
# end

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /usr/local/bin/app /usr/local/bin/app
# if views
COPY --from=build /app/dist ./dist
ENV OPENSHOP_UI_DIR=/app/dist
# end
COPY migrations ./migrations
COPY shopify.app*.toml ./
ENV OPENSHOP_ENV=production PORT=3000
EXPOSE 3000

# Web process. Run workers from the same image with `app worker`,
# and apply migrations before each deploy with `app migrate`.
CMD ["app", "start"]
