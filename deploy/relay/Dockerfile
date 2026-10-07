# Build from the repo root:
#   docker build -f deploy/relay/Dockerfile -t lanlink-relay .
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p lanlink-relay --locked

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/lanlink-relay /usr/local/bin/lanlink-relay
EXPOSE 3340/tcp
ENTRYPOINT ["/usr/local/bin/lanlink-relay"]
CMD ["--http-addr", "0.0.0.0:3340"]
