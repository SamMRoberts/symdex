use std::fs;

pub fn parse_config() -> String {
    read_file()
}

fn read_file() -> String {
    fs::read_to_string("symdex.toml").unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config() {
        let _value = parse_config();
    }
}