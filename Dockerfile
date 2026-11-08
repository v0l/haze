FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release -p pingwatch

FROM scratch
COPY --from=build /src/target/release/pingwatch /pingwatch
WORKDIR /data
ENTRYPOINT ["/pingwatch"]
CMD ["run", "/config.toml"]
