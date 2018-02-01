FROM rust:latest

RUN apt-get update && apt-get -y install openssl cmake libclang-dev clang
