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

    let mut client = Context::new(
        &([0, 0, 0, 0], 0).into(),
        &evt_loop.handle(),
        config,
    ).unwrap();

    let mut con = evt_loop
        .run(client.new_connection(([127, 0, 0, 1], 22222).into(), "server.test"))
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
