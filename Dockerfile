FROM node:22-bookworm-slim AS web
WORKDIR /build/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:bookworm AS source
WORKDIR /build
COPY . .

FROM source AS build-release
ARG CARGO_FEATURES=""
RUN test -z "$CARGO_FEATURES" && cargo build --locked --release -p katwork-server

FROM source AS build-smoke
ARG CARGO_FEATURES="smoke-fixtures"
RUN test "$CARGO_FEATURES" = smoke-fixtures && cargo build --locked --release -p katwork-server --features smoke-fixtures

# The artifact volume's mount point. The runtime image has no shell to make it, and Docker gives
# a new named volume the owner of the directory it mounts over, so it belongs to the runtime's
# non-root user (65532, distroless's nonroot).
FROM rust:bookworm AS artifact-dir
RUN install -d -m 0700 /artifacts

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
WORKDIR /app
COPY --from=web /build/web/dist /app/web/dist
COPY --from=artifact-dir --chown=65532:65532 /artifacts /var/lib/katwork/artifacts
COPY LICENSE NOTICE /app/
EXPOSE 3000
ENTRYPOINT ["/app/katwork-server"]
CMD ["serve"]

FROM runtime AS smoke
COPY --from=build-smoke /build/target/release/katwork-server /app/katwork-server

FROM runtime AS release
COPY --from=build-release /build/target/release/katwork-server /app/katwork-server
