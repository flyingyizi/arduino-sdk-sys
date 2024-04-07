
#[cfg(feature = "prettify_bindgen")]
extern crate clang;


use std::{env, };

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/build_util_for_arduino.rs"));

fn main() {
    main_entry();
    return;
}

