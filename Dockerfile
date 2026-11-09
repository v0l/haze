FROM oven/bun:1 AS dashboard
WORKDIR /ui
COPY crates/web/ui/package.json crates/web/ui/bun.lock ./
RUN bun install --frozen-lockfile
COPY crates/web/ui ./
RUN bun run build

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev libcap-setcap
WORKDIR /src
COPY . .
COPY --from=dashboard /ui/dist/index.html crates/web/ui/dist/index.html
RUN cargo build --release -p haze \
    && cp target/release/haze /haze \
    && setcap cap_net_raw=p /haze \
    && mkdir -p /rootfs/data \
    && chown 65532:65532 /rootfs/data

FROM scratch
COPY --from=build /haze /haze
COPY --from=build --chown=65532:65532 /rootfs/data /data
USER 65532:65532
WORKDIR /data
ENV HAZE_CONFIG=/haze.toml HAZE_DATA=/data HAZE_LISTEN=0.0.0.0:8480
EXPOSE 8480
VOLUME /data
ENTRYPOINT ["/haze"]
CMD ["run"]
