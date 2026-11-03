# The base images are named by digest, the tag only says what it is: a build
# does not move with the tag. Dependabot proposes the next digest.
FROM rust:1-bookworm@sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8 AS build
# Cargo fetches the `openshop` dependency with git, which reads the optional
# build secret `gitconfig`: a `url.<base>.insteadOf` that adds a token, for a
# repository that is private. Without the secret git reads no configuration,
# and the repository has to be public.
#   docker build --secret id=gitconfig,src=/path/outside/the/project/gitconfig .
# The file stays outside the project: `COPY . .` would copy it into a layer.
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true GIT_CONFIG_GLOBAL=/run/secrets/gitconfig
# if views
# What builds the admin UI bundle: the WebAssembly target, trunk, and the
# openshop CLI of the tag, branch or rev of openshop that Cargo.toml names.
RUN rustup target add wasm32-unknown-unknown
# trunk is checked against the checksums its v0.21.14 release publishes.
RUN set -eu; \
    case "$(uname -m)" in \
      x86_64) sha256=f2b4680cd239693a646a2795e4633c625328d7b2a044fbe749fa3a2fe9e7036b ;; \
      aarch64) sha256=b1d8e60e454f7fc182d9a4d95d1506ffbae947d8ba90f8f6f02da93b60f980f9 ;; \
      *) echo "No trunk for $(uname -m)" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/trunk.tar.gz "https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-$(uname -m)-unknown-linux-gnu.tar.gz"; \
    echo "$sha256  /tmp/trunk.tar.gz" | sha256sum --check --strict -; \
    tar -xzf /tmp/trunk.tar.gz -C /usr/local/bin trunk; \
    rm /tmp/trunk.tar.gz
COPY Cargo.toml /tmp/openshop-cli/Cargo.toml
RUN --mount=type=secret,id=gitconfig,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
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
RUN --mount=type=secret,id=gitconfig,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    openshop build $(test -f Cargo.lock && echo --locked) && cp target/release/__PACKAGE_NAME__ /usr/local/bin/app
# else
RUN --mount=type=secret,id=gitconfig,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release $(test -f Cargo.lock && echo --locked) && cp target/release/__PACKAGE_NAME__ /usr/local/bin/app
# end

FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
# The app reads what is copied below and writes nothing: it does not run as root.
RUN groupadd --system --gid 10001 app \
    && useradd --system --uid 10001 --gid app --no-create-home --shell /usr/sbin/nologin app
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
USER app

# Web process. Run workers from the same image with `app worker`,
# and apply migrations before each deploy with `app migrate`.
CMD ["app", "start"]
