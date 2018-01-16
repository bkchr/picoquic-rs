extern crate cmake;
extern crate bindgen;

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    let build_dir = cmake::Config::new("src/picotls")
        .build_target("all")
        .build();

    fs::read_dir(build_dir.join("build"))
        .expect("build_dir not found")
        .for_each(|e| {
            if let Ok(e) = e {
                if e.path().extension().map(|e| e == "a").unwrap_or(false) {
                    fs::copy(
                        e.path(),
                        out_path
                            .join(e.path().file_name().unwrap()),
                    ).expect("error copying library");
                }
            }
        });

    let build_dir = cmake::Config::new("src/picoquic")
        .define("CMAKE_LIBRARY_PATH", out_path.to_str().unwrap())
        .build_target("picoquic-core")
        .build();

    println!(
        "cargo:rustc-link-search=native={}/build/",
        build_dir.display()
    );
    println!("cargo:rustc-link-lib=static=picoquic-core");

    // generate the rust bindings for the picoquic
    let bindings = bindgen::Builder::default()
        .header("src/picoquic/picoquic/picoquic.h")
        .generate()
        .expect("Unable to generate picoquic bindings");

    bindings
        .write_to_file(out_path.join("picoquic.rs"))
        .expect("Couldn't write bindings!");
}
