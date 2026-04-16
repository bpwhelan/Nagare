# ── Stage 1: Build Svelte frontend ───────────────────────────
FROM node:22-alpine AS frontend-build
WORKDIR /app/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# ── Stage 2: Build Rust backend ─────────────────────────────
FROM rust:1.87-alpine AS backend-build
RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs ./
COPY src/ src/
COPY --from=frontend-build /app/frontend/dist ./frontend/dist
RUN cargo build --release

# ── Stage 3: Runtime ────────────────────────────────────────
FROM alpine:3.21
RUN apk add --no-cache ffmpeg ca-certificates
WORKDIR /app
COPY --from=backend-build /app/target/release/nagare /app/nagare
COPY --from=frontend-build /app/frontend/dist /app/frontend/dist

ENV FRONTEND_DIR=/app/frontend/dist
ENV RUST_LOG=info

EXPOSE 9470
CMD ["/app/nagare"]
