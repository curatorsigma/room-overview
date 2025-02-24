FROM rust:1.80-alpine AS builder
RUN apk add --no-cache build-base
WORKDIR /usr/src/room-overview
COPY . .
# we want to compile with offline checking
ENV SQLX_OFFLINE=true
RUN cargo build --release
CMD ["room-overview"]

FROM alpine:latest
WORKDIR /room-overview
COPY --from=builder /usr/src/room-overview/target/release/room-overview ./
CMD ["./room-overview"]

