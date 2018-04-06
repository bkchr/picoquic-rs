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
extern crate tokio_core;

use picoquic::{Config, Context};

use tokio_core::reactor::Core;

use bytes::BytesMut;

use futures::{Future, Sink, Stream};

fn main() {
    let mut evt_loop = Core::new().unwrap();

    let config = Config::new();

    let mut client = Context::new(&([0, 0, 0, 0], 0).into(), &evt_loop.handle(), config).unwrap();

    let mut con = evt_loop
        .run(client.new_connection(([127, 0, 0, 1], 22222).into()))
        .unwrap();

    let stream = evt_loop.run(con.new_bidirectional_stream()).unwrap();

    let stream = evt_loop
        .run(stream.send(BytesMut::from("hello server")))
        .unwrap();

    let answer = evt_loop
        .run(
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
extern crate tokio_core;

use picoquic::{Config, Context};

use tokio_core::reactor::Core;

use futures::{Future, Sink, Stream};

use bytes::BytesMut;

fn main() {
    let mut evt_loop = Core::new().unwrap();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    let mut config = Config::new();
    config.set_cert_chain_filename(format!("{}/examples/cert.pem", manifest_dir));
    config.set_key_filename(format!("{}/examples/key.pem", manifest_dir));

    let server = Context::new(&([0, 0, 0, 0], 22222).into(), &evt_loop.handle(), config).unwrap();

    println!("Server listening on: {}", server.local_addr());

    let handle = evt_loop.handle();

    evt_loop
        .run(server.for_each(|c| {
            let handle = handle.clone();

            println!("New connection from: {}", c.peer_addr());

            handle.clone().spawn(c.for_each(move |s| {
                // We print the received message and sent a new one, after that we collect all
                // remaining messages. The collect is a "hack" that prevents that the `Stream` is
                // dropped to early.
                handle.clone().spawn(
                    s.into_future()
                        .map_err(|_| ())
                        .and_then(|(m, s)| {
                            println!("Got: {:?}", m);
                            s.send(BytesMut::from("hello client")).map_err(|_| ())
                        })
                        .and_then(|s| s.collect().map_err(|_| ()))
                        .map(|_| ()),
                );
                Ok(())
            }).map_err(|_| ()));

            Ok(())
        }))
        .unwrap();
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
extern crate bytes;
extern crate failure;
#[macro_use]
extern crate failure_derive;
#[macro_use]
extern crate futures;
extern crate libc;
#[macro_use]
extern crate log;
extern crate openssl;
extern crate openssl_sys;
extern crate picoquic_sys;
extern crate socket2;
#[macro_use]
extern crate tokio_core;

mod config;
mod connection;
mod context;
mod context_inner;
mod error;
mod ffi;
mod stream;
mod verify_certificate;

pub use self::config::{Config, FileFormat, Role};
pub use self::connection::{Connection, Id as ConnectionId, NewStreamFuture, NewStreamHandle,
                           Type as ConnectionType};
pub use self::context::Context;
pub use self::context_inner::{NewConnectionFuture, NewConnectionHandle};
pub use self::error::{Error, ErrorKind};
pub use self::stream::{Stream, Type as SType};
pub use self::verify_certificate::{default_verify_certificate, VerifyCertificate};
