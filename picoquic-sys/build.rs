extern crate bindgen;
extern crate cc;
extern crate cmake;
extern crate glob;

use std::env;
use std::path::PathBuf;

fn main() {
    // build picotls
    cc::Build::new()
        .flag("-Wno-unused-parameter")
        .flag("-Wno-missing-field-initializers")
        .flag("-Wno-sign-compare")
        .file("src/picotls/lib/picotls.c")
        .file("src/picotls/lib/pembase64.c")
        .file("src/picotls/lib/openssl.c")
        .include("src/picotls/include/")
        .compile("picotls");

    // build picoquic
    cc::Build::new()
        .flag("-Wno-unused-parameter")
        .flag("-Wno-sign-compare")
        .flag("-Wno-unused-but-set-variable")
        .flag("-Wno-missing-field-initializers")
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
        .clang_arg("-DNULL=0")
        .header("src/picotls/include/picotls.h")
        .header("src/picoquic/picoquic/picoquic.h")
        .header("src/picoquic/picoquic/util.h")
        .generate()
        .expect("Unable to generate picoquic bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("picoquic.rs"))
        .expect("Couldn't write bindings!");
}
