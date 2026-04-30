pub fn public_value() -> &'static str {
    "safe-placeholder"
}

pub fn sensitive_value() -> &'static str {
    let api_key = "abcdefghijklmnopqrstuvwxyz123456";
    api_key
}
