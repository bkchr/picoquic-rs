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
