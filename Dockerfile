FROM rust:1.80-alpine AS builder
RUN apk add --no-cache build-base
WORKDIR /usr/src/ct-ta-sync
COPY . .
# we want to compile with offline checking
ENV SQLX_OFFLINE=true
RUN cargo build --release
CMD ["ct-ta-sync"]

FROM alpine:latest
WORKDIR /ct-ta-sync
COPY --from=builder /usr/src/ct-ta-sync/target/release/ct-ta-sync ./
CMD ["./ct-ta-sync"]

