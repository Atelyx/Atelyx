# 构建阶段（alpine + musl，静态链接二进制）
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

# 运行阶段（最小 alpine，单二进制 ~5MB）
FROM alpine:3.20
RUN apk add --no-cache ca-certificates
COPY --from=build /app/target/release/collab-relay /usr/local/bin/collab-relay
EXPOSE 11224
CMD ["collab-relay"]
