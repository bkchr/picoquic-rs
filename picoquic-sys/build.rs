extern crate bindgen;
extern crate cc;
extern crate cmake;
extern crate glob;

use std::env;
use std::path::PathBuf;

fn main() {
    // build picotls
    cc::Build::new()
        .file("src/picotls/lib/picotls.c")
        .file("src/picotls/lib/pembase64.c")
        .file("src/picotls/lib/openssl.c")
        .include("src/picotls/include/")
        .compile("picotls");

    // build picoquic
    cc::Build::new()
        .files(
            glob::glob("src/picoquic/picoquic/*.c")
                .expect("failed to find picoquic c files")
                .filter_map(|p| match p {
                    Ok(p) => Some(p),
                    _ => None,
                }),
        )
        .include("src/picoquic/picoquic")
        .include("src/picotls/include/")
        .compile("picoquic");

    // generate the rust bindings for the picoquic
    let bindings = bindgen::Builder::default()
        .header("src/picoquic/picoquic/picoquic.h")
        .generate()
        .expect("Unable to generate picoquic bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("picoquic.rs"))
        .expect("Couldn't write bindings!");
}
