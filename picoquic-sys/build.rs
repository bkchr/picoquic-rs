extern crate bindgen;
extern crate cc;
extern crate cmake;
extern crate glob;

use std::env;
use std::path::PathBuf;

fn main() {
    let debug = match env::var("DEBUG").ok() {
        Some(val) => val != "false",
        None => false,
    };

    let openssl_include =
        env::var("DEP_OPENSSL_INCLUDE").expect("Could not find openssl include directory.");

    // build picotls
    cc::Build::new()
        .flag("-Wno-unused-parameter")
        .flag("-Wno-missing-field-initializers")
        .flag("-Wno-sign-compare")
        .file("src/picotls/lib/picotls.c")
        .file("src/picotls/lib/pembase64.c")
        .file("src/picotls/lib/openssl.c")
        .include("src/picotls/include/")
        .include(&openssl_include)
        .compile("picotls");

    // build picoquic
    let mut picoquic = cc::Build::new();
    picoquic
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
        .include(openssl_include)
        .include("src/picoquic/picoquic")
        .include("src/picotls/include/");

    if !debug {
        picoquic.define("DISABLE_DEBUG_PRINTF", None);
    }

    picoquic.compile("picoquic");

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
