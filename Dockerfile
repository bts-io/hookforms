# syntax=docker/dockerfile:1

# Build a static musl binary (SQLite is bundled, TLS is rustls: no system libs).
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
    && cp target/release/hookforms /hookforms

FROM alpine:3
RUN addgroup -g 1000 app \
    && adduser -D -H -u 1000 -G app app \
    && mkdir -p /data /app/config/gmail \
    && chown -R app:app /data /app
COPY --from=build /hookforms /usr/local/bin/hookforms
USER app
WORKDIR /app
ENV DATABASE_URL=sqlite:///data/hookforms.db
VOLUME ["/data"]
EXPOSE 8000
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD wget -qO- http://127.0.0.1:8000/health >/dev/null || exit 1
CMD ["hookforms"]
