FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app

# Compile dependencies as a separate layer so they are only rebuilt when
# Cargo.toml or Cargo.lock changes, not on every source edit.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src target/release/deps/rawrr*

COPY src ./src
RUN cargo build --release

FROM alpine:3.21
RUN apk add --no-cache ca-certificates
COPY --from=builder /app/target/release/rawrr /usr/local/bin/
ENTRYPOINT ["rawrr"]
