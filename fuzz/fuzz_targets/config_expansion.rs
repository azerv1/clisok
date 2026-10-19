#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    let _ = clisok::parsing::expand(data, &|key| match key {
        "HOME" => Some("/fixture".into()),
        "EMPTY" => Some(String::new()),
        _ => None,
    });
});
