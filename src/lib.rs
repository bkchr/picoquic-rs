/*!
# Picoquic-rs - Tokio aware bindings of [picoquic](https://github.com/private-octopus/picoquic)
[![](https://docs.rs/picoquic/badge.svg)](https://docs.rs/picoquic/) [![](https://img.shields.io/crates/v/picoquic.svg)](https://crates.io/crates/picoquic) [![](https://img.shields.io/crates/d/picoquic.png)](https://crates.io/crates/picoquic) [![Build Status](https://travis-ci.org/bkchr/picoquic-rs.png?branch=master)](https://travis-ci.org/bkchr/picoquic-rs)

Picoquic is a minimalist implementation of the QUIC protocol by the IETF. The protocol is still
in [development](https://github.com/quicwg) and so the implementation.

* [Building](#building)
* [Example](#example)
* [Todo](#todo)
* [License](#license)
* [Contribution](#contribution)

## Building

For building picoquic-rs, you need the following dependencies:
* clang
* openssl

Building is currently only tested on Linux. To build the project, you just need to
run `cargo build`.
`picoquic-sys` will also build the `picoquic` c-library for you (hopefully).

## Example

### Client
```no_run
extern crate bytes;
extern crate futures;
extern crate picoquic;
extern crate tokio;

use picoquic::{Config, Context};

use bytes::Bytes;

use futures::{Future, Sink, Stream};

fn main() {
    let mut evt_loop = tokio::runtime::Runtime::new().unwrap();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    let mut config = Config::new();
    config.set_root_certificate_filename(format!("{}/examples/ca_cert.pem", manifest_dir));

    let mut client = Context::new(&([0, 0, 0, 0], 0).into(), evt_loop.executor(), config).unwrap();

    let mut con = evt_loop
        .block_on(client.new_connection(([127, 0, 0, 1], 22222).into(), "server.test"))
        .unwrap();

    let stream = evt_loop.block_on(con.new_bidirectional_stream()).unwrap();

    let stream = evt_loop
        .block_on(stream.send(Bytes::from("hello server")))
        .unwrap();

    let answer = evt_loop
        .block_on(
            stream
                .into_future()
                .map(|(m, _)| m.unwrap())
                .map_err(|(e, _)| e),
        )
        .unwrap();

    println!("Got: {:?}", answer);
}
```

### Server
```no_run
extern crate bytes;
extern crate futures;
extern crate picoquic;
extern crate tokio;

use picoquic::{Config, Context};

use futures::{Future, Sink, Stream};

use bytes::Bytes;

fn main() {
    let evt_loop = tokio::runtime::Runtime::new().unwrap();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    let mut config = Config::new();
    config.set_certificate_chain_filename(format!("{}/examples/cert.pem", manifest_dir));
    config.set_private_key_filename(format!("{}/examples/key.pem", manifest_dir));

    let server = Context::new(&([0, 0, 0, 0], 22222).into(), evt_loop.executor(), config).unwrap();

    println!("Server listening on: {}", server.local_addr());

    evt_loop.block_on_all(
        server
            .for_each(|c| {
                println!("New connection from: {}", c.peer_addr());

                tokio::spawn(
                    c.for_each(move |s| {
                        // We print the received message and sent a new one, after that we collect all
                        // remaining messages. The collect is a "hack" that prevents that the `Stream` is
                        // dropped too early.
                        tokio::spawn(
                            s.into_future()
                                .map_err(|_| ())
                                .and_then(|(m, s)| {
                                    println!("Got: {:?}", m);
                                    s.send(Bytes::from("hello client")).map_err(|_| ())
                                })
                                .and_then(|s| s.collect().map_err(|_| ()))
                                .map(|_| ()),
                        );
                        Ok(())
                    })
                    .map_err(|_| ()),
                );

                Ok(())
            })
    ).unwrap();
}
```

## Todo

* My first crate/project that uses `failure` and I'm not happy with the current error structure :(
* Support more configuration options
* I currently don't check all return codes of the c functions.
* Remove the TODOs from the source code

## License

Licensed under either of

 * Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
*/

#[macro_use]
extern crate futures;
#[macro_use]
extern crate log;

#[macro_use]
mod error;
mod config;
mod connection;
mod context;
mod context_inner;
mod ffi;
mod stream;
mod channel_with_error;
mod verify_certificate;
mod swappable_stream;

pub use self::config::{Config, FileFormat, Role};
pub use self::connection::{
    Connection, Id as ConnectionId, NewStreamFuture, NewStreamHandle, Type as ConnectionType,
};
pub use self::context::Context;
pub use self::context_inner::{NewConnectionFuture, NewConnectionHandle};
pub use self::error::{Error, ErrorKind};
pub use self::stream::{Stream, Type as SType};
pub use self::verify_certificate::{default_verify_certificate, VerifyCertificate};
