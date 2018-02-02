FROM rust:latest

RUN apt-get update && apt-get -y install openssl libclang-dev clang
