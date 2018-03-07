extern crate bytes;
extern crate futures;
extern crate picoquic;
extern crate timebomb;
extern crate tokio_core;

use picoquic::{Config, Connection, Context, ErrorKind, NewStreamFuture, NewStreamHandle, SType,
               Stream};

use std::net::SocketAddr;
use std::thread;
use std::sync::mpsc::channel;
use std::fmt;
use std::sync::Arc;

use futures::{Future, Sink, Stream as FStream};
use futures::sync::mpsc::unbounded;

use tokio_core::reactor::{Core, Handle};

use bytes::BytesMut;

fn get_test_config() -> Config {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    Config::server(
        &format!("{}/tests/certs/device.test.crt", manifest_dir),
        &format!("{}/tests/certs/device.key", manifest_dir),
    )
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
    F: 'static + Send + FnOnce(Context, Handle) -> R,
    R: Future,
    <R as Future>::Error: fmt::Debug,
{
    let (send, recv) = channel();

    thread::spawn(move || {
        let (context, mut evt_loop) = create_context_and_evt_loop();

        send.send(context.local_addr())
            .expect("sends server socket addr");

        let handle = evt_loop.handle();
        evt_loop
            .run(create_future(context, handle))
            .expect("event loop spins on server context");
    });

    recv.recv().expect("receives server socket addr")
}

#[test]
fn server_start() {
    start_server_thread(|c, _| c.for_each(|_| Ok(())));
}

fn client_connects_creates_stream_and_sends_data<F, T>(create_stream: F, check_type: T)
where
    F: Fn(Connection) -> (NewStreamFuture, Connection),
    T: Fn(&Stream),
{
    let send_data = "hello server";
    let (send, recv) = unbounded();

    let addr = start_server_thread(move |c, _| {
        c.for_each(move |c| {
            let send = send.clone();
            c.for_each(move |s| {
                let send = send.clone();
                s.for_each(move |m| {
                    let _ = send.clone()
                        .unbounded_send(String::from_utf8(m.to_vec()).unwrap());
                    Ok(())
                })
            })
        })
    });

    let (mut context, mut evt_loop) = create_context_and_evt_loop();

    let con = evt_loop
        .run(context.new_connection(([127, 0, 0, 1], addr.port()).into()))
        .expect("creates connection");

    let (new_stream, _con) = create_stream(con);
    let stream = evt_loop.run(new_stream).expect("creates stream");
    check_type(&stream);

    assert_eq!(
        stream.local_addr(),
        ([0, 0, 0, 0], context.local_addr().port()).into()
    );
    assert_eq!(stream.peer_addr(), ([127, 0, 0, 1], addr.port()).into());
    assert_ne!(stream.peer_addr(), stream.local_addr());

    // The stream must not be dropped here!
    let _stream = evt_loop
        .run(stream.send(BytesMut::from(send_data)))
        .unwrap();

    assert_eq!(
        send_data,
        evt_loop.run(recv.into_future()).unwrap().0.unwrap()
    );
}

#[test]
fn client_connects_creates_bidirectional_stream_and_sends_data() {
    client_connects_creates_stream_and_sends_data(
        |mut c| {
            let stream = c.new_bidirectional_stream();
            (stream, c)
        },
        |s| {
            assert!(match s.get_type() {
                SType::Bidirectional => true,
                _ => false,
            })
        },
    )
}

#[test]
fn client_connects_creates_unidirectional_stream_and_sends_data() {
    client_connects_creates_stream_and_sends_data(
        |mut c| {
            let stream = c.new_unidirectional_stream();
            (stream, c)
        },
        |s| {
            assert!(match s.get_type() {
                SType::Unidirectional => true,
                _ => false,
            })
        },
    )
}

#[test]
fn connection_and_stream_closes_on_drop() {
    timebomb::timeout_ms(connection_and_stream_closes_on_drop_inner, 10000);
}

fn connection_and_stream_closes_on_drop_inner() {
    let send_data = "hello server";
    let (send, recv) = unbounded();

    let addr = start_server_thread(move |c, _| {
        c.for_each(move |c| {
            let send = send.clone();
            c.into_future().map_err(|e| e.0).and_then(move |(s, c)| {
                let send = send.clone();
                s.unwrap()
                    .into_future()
                    .map(move |_| {
                        let _ = send.clone().unbounded_send(true);
                        let _ = c;
                        ()
                    })
                    .map_err(|e| e.0)
            })
        })
    });

    let (mut context, mut evt_loop) = create_context_and_evt_loop();

    let mut con = evt_loop
        .run(context.new_connection(([127, 0, 0, 1], addr.port()).into()))
        .expect("creates connection");

    let stream = evt_loop
        .run(con.new_bidirectional_stream())
        .expect("creates stream");

    let stream = evt_loop
        .run(stream.send(BytesMut::from(send_data)))
        .unwrap();

    assert!(evt_loop.run(recv.into_future()).unwrap().0.unwrap());

    assert!(match evt_loop
        .run(stream.into_future().map(|(m, _)| m).map_err(|(e, _)| e))
        .unwrap()
    {
        None => true,
        _ => false,
    });

    assert!(match evt_loop
        .run(con.into_future().map(|(m, _)| m).map_err(|(e, _)| e))
        .unwrap()
    {
        None => true,
        _ => false,
    });
}

#[test]
fn open_multiple_streams_sends_data_and_recvs() {
    let send_data = "hello server";
    let expected_stream_count = 4;

    let addr = start_server_thread(move |c, h| {
        c.for_each(move |c| {
            let h = h.clone();

            h.clone().spawn(c.for_each(move |s| {
                // Peer addr and local addr can not be the same
                assert_ne!(s.peer_addr(), s.local_addr());
                // The local address should be unspecified, as we do not set a specific address in
                // the tests.
                assert!(s.local_addr().ip().is_unspecified());

                h.clone().spawn(
                    s.into_future()
                        .map_err(|_| ())
                        .and_then(|(v, s)|{
                           s.send(v.unwrap())
                            .map_err(|_| ()) })
                        // we need to do a fake collect here, to prevent that the stream gets
                        // dropped to early.
                        .and_then(|s| s.collect().map_err(|_| ()))
                        .map(|_| ()),
                );
                Ok(())
            }).map_err(|_| ()));

            Ok(())
        })
    });

    let (mut context, mut evt_loop) = create_context_and_evt_loop();

    let mut con = evt_loop
        .run(context.new_connection(([127, 0, 0, 1], addr.port()).into()))
        .expect("creates connection");

    let mut streams = Vec::new();

    for i in 0..expected_stream_count {
        let stream = evt_loop
            .run(con.new_bidirectional_stream())
            .expect("creates stream");
        streams.push(
            evt_loop
                .run(stream.send(BytesMut::from(format!("{}{}", send_data, i))))
                .unwrap(),
        );
    }

    for (i, stream) in streams.iter_mut().enumerate() {
        assert_eq!(
            format!("{}{}", send_data, i),
            String::from_utf8(
                evt_loop
                    .run(stream.into_future().map(|(m, _)| m).map_err(|(e, _)| e))
                    .unwrap()
                    .unwrap()
                    .to_vec()
            ).unwrap()
        );
    }
}

fn open_stream_to_server_and_server_creates_new_stream_to_answer<F>(create_stream: F)
where
    F: 'static + Sync + Send + Fn(NewStreamHandle) -> NewStreamFuture,
{
    let create_stream = Arc::new(Box::new(create_stream));
    let send_data = "hello server";

    let create_stream2 = create_stream.clone();
    let addr = start_server_thread(move |c, _| {
        let create_stream = create_stream2;
        c.for_each(move |c| {
            let create_stream = create_stream.clone();
            let new_stream = c.get_new_stream_handle();

            c.for_each(move |s| {
                let create_stream = create_stream.clone();
                let new_stream = new_stream.clone();

                s.into_future()
                        .map_err(|e| e.0)
                        .and_then(move |(v, _)|
                                  create_stream(new_stream.clone())
                                  .and_then(move |s|
                                            s.send(v.unwrap())
                                            .map_err(|_| ErrorKind::Unknown.into())))
                        // we need to do a fake collect here, to prevent that the stream gets
                        // dropped to early.
                    .and_then(|s| s.collect().map_err(|_| ErrorKind::Unknown.into()))
                        .map(|_| ())
            })
        })
    });

    let (mut context, mut evt_loop) = create_context_and_evt_loop();

    let con = evt_loop
        .run(context.new_connection(([127, 0, 0, 1], addr.port()).into()))
        .expect("creates connection");

    let stream = evt_loop
        .run(create_stream(con.get_new_stream_handle()))
        .expect("creates stream");
    let _stream = evt_loop
        .run(stream.send(BytesMut::from(send_data)))
        .unwrap();

    let (new_stream, _con) = evt_loop
        .run(
            con.into_future()
                .map(|(m, c)| (m.unwrap(), c))
                .map_err(|(e, _)| e),
        )
        .unwrap();

    assert_eq!(
        send_data,
        String::from_utf8(
            evt_loop
                .run(new_stream.into_future().map(|(m, _)| m).map_err(|(e, _)| e))
                .unwrap()
                .unwrap()
                .to_vec()
        ).unwrap()
    );
}

#[test]
fn open_uni_stream_to_server_and_server_creates_new_uni_stream_to_answer() {
    open_stream_to_server_and_server_creates_new_stream_to_answer(|mut h| {
        h.new_unidirectional_stream()
    });
}

#[test]
fn open_bi_stream_to_server_and_server_creates_new_bi_stream_to_answer() {
    open_stream_to_server_and_server_creates_new_stream_to_answer(|mut h| {
        h.new_bidirectional_stream()
    });
}

// Regression test for `ffi::ConnectionIter`.
// We observed that the connection iterator could loop infinitely, because picoquic reorders
// connections internally while working with them. The following test should detect this bug.
#[test]
fn open_multiple_connections() {
    timebomb::timeout_ms(
        || {
            let (mut context, mut evt_loop) = create_context_and_evt_loop();

            let con0 = context.new_connection(([127, 0, 0, 1], 4000).into());
            let con1 = context.new_connection(([127, 0, 0, 1], 4001).into());
            let con2 = context.new_connection(([127, 0, 0, 1], 4002).into());
            let con3 = context.new_connection(([127, 0, 0, 1], 4003).into());
            let con4 = context.new_connection(([127, 0, 0, 1], 4004).into());

            evt_loop.run(con0.join5(con1, con2, con3, con4)).unwrap();
        },
        10000,
    );
}
