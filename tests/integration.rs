extern crate bytes;
extern crate futures;
extern crate picoquic;
extern crate tokio_core;

use picoquic::{Config, Context, SMessage};

use std::net::SocketAddr;
use std::thread;
use std::sync::mpsc::channel;
use std::fmt;

use futures::{Future, Sink, Stream as FStream};
use futures::sync::mpsc::unbounded;

use tokio_core::reactor::Core;

use bytes::Bytes;

fn get_test_config() -> Config {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    Config {
        cert_filename: format!("{}/tests/cert.pem", manifest_dir),
        key_filename: format!("{}/tests/key.pem", manifest_dir),
        reset_seed: None,
    }
}

fn create_context_and_evt_loop() -> (Context, Core) {
    let evt_loop = Core::new().expect("creates event loop");

    let context = Context::new(
        &([0, 0, 0, 0], 0).into(),
        &evt_loop.handle(),
        get_test_config(),
    ).expect("creates quic context");

    (context, evt_loop)
}

fn start_server_thread<F, R>(create_future: F) -> SocketAddr
where
    F: 'static + Send + FnOnce(Context) -> R,
    R: Future,
    <R as Future>::Error: fmt::Debug,
{
    let (send, recv) = channel();

    thread::spawn(move || {
        let (context, mut evt_loop) = create_context_and_evt_loop();

        send.send(context.local_addr())
            .expect("sends server socket addr");

        evt_loop
            .run(create_future(context))
            .expect("event loop spins on server context");
    });

    recv.recv().expect("receives server socket addr")
}

#[test]
fn server_start() {
    start_server_thread(|c| c.for_each(|_| Ok(())));
}

#[test]
fn client_connects_creates_stream_and_sends_data() {
    let (send, recv) = unbounded();

    let addr = start_server_thread(move |c| {
        c.for_each(move |_| {
            let _ = send.clone().send(true);
            Ok(())
        })
    });

    let (mut context, mut evt_loop) = create_context_and_evt_loop();

    let mut con = evt_loop
        .run(context.connect_to(addr))
        .expect("creates connection");

    let stream = evt_loop
        .run(con.new_bidirectional_stream())
        .expect("creates stream");

    eprintln!("WAIT");
    evt_loop
        .run(stream.send(SMessage::Data(Bytes::from("hello server"))))
        .unwrap();

    assert!(evt_loop.run(recv.into_future()).unwrap().0.unwrap());
}
