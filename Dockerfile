FROM oven/bun:1 AS dashboard
WORKDIR /ui
COPY crates/web/ui/package.json crates/web/ui/bun.lock ./
RUN bun install --frozen-lockfile
COPY crates/web/ui ./
RUN bun run build

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
COPY --from=dashboard /ui/dist/index.html crates/web/ui/dist/index.html
RUN cargo build --release -p haze

FROM scratch
COPY --from=build /src/target/release/haze /haze
WORKDIR /data
ENV HAZE_CONFIG=/haze.toml HAZE_DATA=/data HAZE_LISTEN=0.0.0.0:8480
EXPOSE 8480
ENTRYPOINT ["/haze"]
CMD ["run"]
