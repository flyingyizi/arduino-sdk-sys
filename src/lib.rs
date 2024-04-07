#![no_std]


pub use bindings::*;

    #[cfg(all(target_arch = "avr", feature = "native_bindgen"))]
    #[allow(non_camel_case_types,dead_code)]
    #[path ="rust_avr_ctypes.rs"]
    mod rust_ctypes;

#[allow(clippy::all)]
#[allow(non_upper_case_globals)]
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
// #[allow(rustdoc::all)]
// #[allow(improper_ctypes)]
pub mod bindings {

    #[cfg(feature = "native_bindgen")]
    include!(concat!(env!("OUT_DIR"), "/arduino_sdk_bindings.rs"));
}
