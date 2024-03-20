#![no_std]
#![allow(non_camel_case_types,non_upper_case_globals,non_snake_case)]


// // bindgen uses 'int' for preprocessor defines which causes
// // overflowing literal warnings.
// // avr-rust/libc#1
// #![allow(overflowing_literals)]



// #[cfg(all(target_arch = "avr", ))]
#[path ="rust_avr_ctypes.rs"]
mod rust_ctypes;

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
