# 构建阶段（alpine + musl，静态链接二进制）
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# 管理台页面在编译期嵌入（src/webui.rs 的 include_str!），构建阶段必须可见
COPY assets ./assets
RUN cargo build --release

# 运行阶段（最小 alpine，单二进制 ~5MB）
FROM alpine:3.20
RUN apk add --no-cache ca-certificates
COPY --from=build /app/target/release/atelyx-server /usr/local/bin/atelyx-server
EXPOSE 11224
CMD ["atelyx-server"]
