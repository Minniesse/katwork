FROM node:20-bookworm-slim AS web
WORKDIR /build
COPY . .
RUN mkdir /web-dist && \
    if [ -f web/package.json ]; then \
      cd web && npm ci && npm run build && cp -R dist/. /web-dist/; \
    fi

FROM rust:bookworm AS build
WORKDIR /build
COPY . .
RUN cargo build --locked --release -p katwork-server

FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app
COPY --from=build /build/target/release/katwork-server /app/katwork-server
COPY --from=web /web-dist /app/web/dist
COPY LICENSE NOTICE /app/
EXPOSE 3000
ENTRYPOINT ["/app/katwork-server"]
CMD ["serve"]
