extern crate bytes;
extern crate futures;
extern crate picoquic;
extern crate tokio_core;

use picoquic::{CMessage, Config, Context, SMessage};

use tokio_core::reactor::Core;

use futures::{Future, Sink, Stream};

use bytes::Bytes;

fn main() {
    let mut evt_loop = Core::new().unwrap();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    let config = Config::server(
        &format!("{}/examples/cert.pem", manifest_dir),
        &format!("{}/examples/key.pem", manifest_dir),
    );

    let server = Context::new(&([0, 0, 0, 0], 22222).into(), &evt_loop.handle(), config).unwrap();

    println!("Server listening on: {}", server.local_addr());

    let handle = evt_loop.handle();

    evt_loop
        .run(server.for_each(|c| {
            println!("New connection from: {}", c.peer_addr());

            let handle = handle.clone();
            handle.clone().spawn(c.for_each(move |s| {
                // Let's see what we got
                let s = match s {
                    CMessage::NewStream(s) => s,
                    _ => return Ok(()),
                };

                // We print the received message and sent a new one, after that we collect all
                // remaining messages. The collect is a "hack" that prevents that the `Stream` is
                // dropped to early.
                handle.spawn(
                    s.into_future()
                        .map_err(|_| ())
                        .and_then(|(m, s)| {
                            println!("Got: {:?}", m);
                            s.send(SMessage::Data(Bytes::from("hello client")))
                                .map_err(|_| ())
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
