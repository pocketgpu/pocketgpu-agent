//! Run with `cargo +nightly fuzz run decode` from `crates/proto`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pocketgpu_proto::frame::decode;
use pocketgpu_proto::msg::{control, jobs, metrics, pair};

fuzz_target!(|data: &[u8]| {
    // Decoding untrusted bytes must never panic, whatever the message type.
    let _ = decode::<pair::PairRequest>(data);
    let _ = decode::<pair::Response>(data);
    let _ = decode::<control::Request>(data);
    let _ = decode::<control::Response>(data);
    let _ = decode::<metrics::Subscribe>(data);
    let _ = decode::<metrics::Event>(data);
    let _ = decode::<jobs::Request>(data);
    let _ = decode::<jobs::Response>(data);
});
