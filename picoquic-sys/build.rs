extern crate bindgen;
extern crate cc;
extern crate glob;

use std::env;
use std::path::PathBuf;

fn main() {
    let debug = match env::var("DEBUG").ok() {
        Some(val) => val != "false",
        None => false,
    };

    let target = env::var("TARGET").unwrap();

    let openssl_include = env::var("DEP_OPENSSL_INCLUDE");

    if let Err(_) = openssl_include {
        println!(
            "cargo:warning=Could not find openssl include directory via `DEP_OPENSSL_INCLUDE`."
        )
    }

    // build picotls
    let mut picotls = cc::Build::new();
    picotls
        .flag("-Wno-unused-parameter")
        .flag("-Wno-missing-field-initializers")
        .flag("-Wno-sign-compare")
        .opt_level(1)
        .file("src/picotls/lib/picotls.c")
        .file("src/picotls/lib/pembase64.c")
        .file("src/picotls/lib/openssl.c")
        .file("src/picotls/lib/cifra.c")
        .file("src/picotls/deps/cifra/src/aes.c")
        .file("src/picotls/deps/cifra/src/curve25519.c")
        .file("src/picotls/deps/cifra/src/chacha20.c")
        .file("src/picotls/deps/cifra/src/sha256.c")
        .file("src/picotls/deps/cifra/src/sha512.c")
        .file("src/picotls/deps/cifra/src/poly1305.c")
        .file("src/picotls/deps/cifra/src/drbg.c")
        .file("src/picotls/deps/cifra/src/blockwise.c")
        .include("src/picotls/deps/cifra/src/")
        .include("src/picotls/deps/cifra/src/ext/")
        .include("src/picotls/include/");

    if let Ok(ref openssl_include) = openssl_include {
        picotls.include(&openssl_include);
    }

    if target.contains("android") {
        picotls.flag("-std=c99");
    }

    picotls.compile("picotls");

    // build picoquic
    let mut picoquic = cc::Build::new();
    picoquic
        .flag("-Wno-unused-parameter")
        .flag("-Wno-sign-compare")
        .flag("-Wno-unused-but-set-variable")
        .flag("-Wno-missing-field-initializers")
        .opt_level(1)
        .files(
            glob::glob("src/picoquic/picoquic/*.c")
                .expect("failed to find picoquic c files")
                .filter_map(|p| match p {
                    Ok(p) => Some(p),
                    _ => None,
                }),
        )
        .include("src/picoquic/picoquic")
        .include("src/picotls/include/");

    if let Ok(ref openssl_include) = openssl_include {
        picoquic.include(&openssl_include);
    }

    if target.contains("android") {
        picoquic.flag("-std=c99");
    }

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
        .blacklist_type("sockaddr_storage")
        .blacklist_type("__kernel_sockaddr_storage")
        .raw_line("pub type sockaddr_storage = ::libc::sockaddr_storage;")
        .raw_line("pub type __kernel_sockaddr_storage = sockaddr_storage;")
        .generate()
        .expect("Generates picoquic bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("picoquic.rs"))
        .expect("Couldn't write bindings!");
}
