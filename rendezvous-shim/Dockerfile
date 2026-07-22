# The shim has no dependencies, so this builds with rustc directly — no cargo,
# no crate downloads, a couple of seconds. rust:alpine targets musl and links
# statically by default, so the result runs on `scratch`.
FROM rust:1-alpine AS build
WORKDIR /src
COPY main.rs .
RUN rustc -O -C strip=symbols main.rs -o shim

FROM scratch
COPY --from=build /src/shim /shim
EXPOSE 21126
ENTRYPOINT ["/shim"]
