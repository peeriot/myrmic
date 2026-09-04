FROM rust:1.94-slim-trixie AS builder

COPY myrmic /usr/local/bin/myrmic
