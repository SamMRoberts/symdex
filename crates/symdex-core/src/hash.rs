const FNV_OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
const FNV_PRIME: u128 = 0x0000000001000000000000000000013b;

pub fn content_hash(bytes: &[u8]) -> String {
    stable_hash_with_domain("content", bytes)
}

pub fn stable_id(parts: &[&str]) -> String {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(part.as_bytes());
        bytes.push(0);
    }
    stable_hash_with_domain("id", &bytes)
}

fn stable_hash_with_domain(domain: &str, bytes: &[u8]) -> String {
    let mut hash = FNV_OFFSET;
    for byte in domain.as_bytes().iter().chain(bytes.iter()) {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:032x}")
}

#[cfg(test)]
mod tests {
    use super::{content_hash, stable_id};

    #[test]
    fn content_hash_is_stable() {
        assert_eq!(
            content_hash(b"fn main() {}\n"),
            content_hash(b"fn main() {}\n")
        );
        assert_ne!(content_hash(b"a"), content_hash(b"b"));
    }

    #[test]
    fn stable_ids_are_domain_separated_from_content_hashes() {
        assert_ne!(stable_id(&["abc"]), content_hash(b"abc\0"));
    }
}
