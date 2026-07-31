FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /app

# Compile dependencies as a separate layer so they are only rebuilt when
# Cargo.toml or Cargo.lock changes, not on every source edit.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src target/release/deps/rawrr*

# build.rs is copied here (not in the dep layer above) so the dep cache is
# keyed only on Cargo.toml and Cargo.lock. The dummy main.rs doesn't call
# env!("GIT_COMMIT_HASH"), so build.rs doesn't need to run during dep
# compilation, and neither build.rs changes nor a new commit hash will ever
# bust the dep cache.
COPY src ./src
COPY build.rs ./

ARG GIT_COMMIT_HASH=unknown
RUN GIT_COMMIT_HASH=${GIT_COMMIT_HASH} cargo build --release

FROM alpine:3.21
RUN apk add --no-cache ca-certificates
COPY --from=builder /app/target/release/rawrr /usr/local/bin/
ENTRYPOINT ["rawrr"]
