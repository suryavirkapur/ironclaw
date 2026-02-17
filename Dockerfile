FROM rust:1.86-slim AS builder

WORKDIR /app
COPY . .
RUN cargo build --release -p ironclawd

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/ironclawd /usr/local/bin/ironclawd

EXPOSE 9938
ENV RUST_LOG=info

CMD ["ironclawd"]
