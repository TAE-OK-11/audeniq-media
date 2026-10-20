#![no_main]
use audeniq_photo::{Deadline, Kind};
use libfuzzer_sys::fuzz_target;
use std::time::Duration;

fuzz_target!(|data: &[u8]| {
    let d = || Deadline::after(Duration::from_secs(5));
    let _ = audeniq_photo::probe(data);
    let _ = audeniq_photo::inspect_cover(data, &d());
    let _ = audeniq_photo::sanitize(data, Kind::Jpeg, &d());
    let _ = audeniq_photo::sanitize(data, Kind::Png, &d());
    let _ = audeniq_photo::sanitize(data, Kind::Signature, &d());
    let _ = audeniq_photo::qr_count(data, &d());
});
