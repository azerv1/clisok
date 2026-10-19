#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    if let Some(module) = clisok::parsing::entry_module(data) {
        assert!(!module.is_empty());
        assert_ne!(module, "__future__");
        assert!(!module.contains('.'));
    }
});
