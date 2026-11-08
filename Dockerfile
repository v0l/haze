FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release -p haze

FROM scratch
COPY --from=build /src/target/release/haze /haze
WORKDIR /data
ENTRYPOINT ["/haze"]
CMD ["run", "/config.toml"]
