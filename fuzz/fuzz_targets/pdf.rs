#![no_main]
use audeniq_photo::Deadline;
use libfuzzer_sys::fuzz_target;
use std::time::Duration;

fuzz_target!(|data: &[u8]| {
    let _ = audeniq_photo::pdf::info(data);
    let _ = audeniq_photo::pdf::sanitize_pdf(data, &Deadline::after(Duration::from_secs(5)));
});
