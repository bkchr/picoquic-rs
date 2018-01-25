extern crate futures;
extern crate picoquic;
extern crate tokio_core;

use picoquic::{Config, Server};

use tokio_core::reactor::Core;

use futures::Stream;

fn main() {
    let mut evt_loop = Core::new().unwrap();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    let config = Config {
        cert_filename: format!("{}/examples/cert.pem", manifest_dir),
        key_filename: format!("{}/examples/key.pem", manifest_dir),
        reset_seed: None,
    };

    let server = Server::new(&([0, 0, 0, 0], 0).into(), &evt_loop.handle(), config).unwrap();

    evt_loop
        .run(server.for_each(|c| {
            println!("New connection from: {}", c.peer_addr());
            Ok(())
        }))
        .unwrap();
}
