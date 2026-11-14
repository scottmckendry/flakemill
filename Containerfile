# Fleet update service (PoC)
#
# Runtime needs: nix, git, cachix, minisign. Nix is installed via the
# Determinate installer (works in a container with --init none).
#
# Build:  docker build -t flakemill .
# Run:    docker run -d \
#           -v flakemill-nix:/nix -v fleet-data:/var/lib/flakemill \
#           -v /path/to/minisign.key:/secrets/sign.key:ro \
#           -e SIGN_KEY=/secrets/sign.key \
#           -e SYSTEMS=eris -e CACHIX_CACHE=scottmckendry \
#           -e CACHIX_AUTH_TOKEN=... \
#           -p 8080:8080 flakemill

FROM rust:1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
ENV CARGO_TERM_COLOR=always
RUN cargo build --release

FROM debian:bookworm-slim AS base
RUN apt-get update && apt-get install -y --no-install-recommends \
      curl xz-utils git ca-certificates minisign \
    && rm -rf /var/lib/apt/lists/*

# nix (determinate installer, single-user, no systemd inside a container)
RUN curl -fsSL https://install.determinate.systems/nix \
      | sh -s -- install linux --init none --no-confirm
ENV PATH=/nix/var/nix/profiles/default/bin:/root/.nix-profile/bin:${PATH}

# cachix (no GH releases; install via its official nix expr)
RUN nix-env -iA cachix -f https://cachix.org/api/v1/install

COPY --from=build /build/target/release/flakemill /usr/local/bin/flakemill

# nix needs these dirs even in "container mode" for userless builds
RUN mkdir -p /nix/var/nix/profiles/per-user/root \
    && echo "build-users-group =" >> /etc/nix/nix.conf

# create a seed of the nix store
RUN mv /nix /nix.seed

COPY container/entrypoint.sh /usr/local/bin/entrypoint.sh

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["/usr/local/bin/flakemill"]
