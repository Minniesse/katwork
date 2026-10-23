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

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
WORKDIR /app
COPY --from=web /build/web/dist /app/web/dist
COPY LICENSE NOTICE /app/
EXPOSE 3000
ENTRYPOINT ["/app/katwork-server"]
CMD ["serve"]

FROM runtime AS smoke
COPY --from=build-smoke /build/target/release/katwork-server /app/katwork-server

FROM runtime AS release
COPY --from=build-release /build/target/release/katwork-server /app/katwork-server
