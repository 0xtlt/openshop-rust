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
WORKDIR /app
COPY . .
# `--locked` as soon as the project has its Cargo.lock.
RUN --mount=type=secret,id=gitconfig,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release $(test -f Cargo.lock && echo --locked) && cp target/release/__PACKAGE_NAME__ /usr/local/bin/app

FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
# The app reads what is copied below and writes nothing: it does not run as root.
RUN groupadd --system --gid 10001 app \
    && useradd --system --uid 10001 --gid app --no-create-home --shell /usr/sbin/nologin app
WORKDIR /app
COPY --from=build /usr/local/bin/app /usr/local/bin/app
COPY migrations ./migrations
COPY shopify.app*.toml ./
ENV OPENSHOP_ENV=production PORT=3000
EXPOSE 3000
USER app

# Web process. Run workers from the same image with `app worker`,
# and apply migrations before each deploy with `app migrate`.
CMD ["app", "start"]
