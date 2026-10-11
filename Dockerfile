# syntax=docker/dockerfile:1
# One image, both modes: `docker run IMAGE serve ...` runs the relay, any
# other arguments run the agent. The default target builds from source;
# release.yml builds the `release` target from the binaries CI already built.

FROM node:24-alpine AS ui
WORKDIR /ui
COPY crates/web/ui/package.json crates/web/ui/package-lock.json ./
RUN npm ci
COPY crates/web/ui/ ./
RUN npm run build

FROM rust:1.94.1-alpine AS build
RUN apk add --no-cache build-base cmake perl
WORKDIR /src
COPY . .
COPY --from=ui /ui/dist crates/web/ui/dist
# Same release profile as the CI build.
ENV CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
    CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_STRIP=symbols
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked && cp target/release/vorp /vorp

FROM --platform=$BUILDPLATFORM busybox:1.37 AS dirs
RUN mkdir -p /data/var/lib/vorp

FROM gcr.io/distroless/static-debian12:nonroot AS release
ARG TARGETARCH
COPY --chmod=755 bin/linux-${TARGETARCH}/vorp /vorp
# A fresh named volume copies this ownership, so the nonroot relay can write.
COPY --from=dirs --chown=nonroot:nonroot /data/ /
EXPOSE 443
VOLUME /var/lib/vorp
ENTRYPOINT ["/vorp"]

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /vorp /vorp
# A fresh named volume copies this ownership, so the nonroot relay can write.
COPY --from=dirs --chown=nonroot:nonroot /data/ /
EXPOSE 443
VOLUME /var/lib/vorp
ENTRYPOINT ["/vorp"]
