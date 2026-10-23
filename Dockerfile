FROM node:22-bookworm-slim AS web
WORKDIR /build/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:bookworm AS build
WORKDIR /build
COPY . .
ARG CARGO_FEATURES=""
RUN cargo build --locked --release -p katwork-server --features "$CARGO_FEATURES"

FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app
COPY --from=build /build/target/release/katwork-server /app/katwork-server
COPY --from=web /build/web/dist /app/web/dist
COPY LICENSE NOTICE /app/
EXPOSE 3000
ENTRYPOINT ["/app/katwork-server"]
CMD ["serve"]
