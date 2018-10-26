extern crate bytes;
extern crate futures;
extern crate picoquic;
extern crate tokio;

use picoquic::{Config, Context};

use futures::{Future, Sink, Stream};

use bytes::BytesMut;

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
                                    s.send(BytesMut::from("hello client")).map_err(|_| ())
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
