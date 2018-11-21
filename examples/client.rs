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
