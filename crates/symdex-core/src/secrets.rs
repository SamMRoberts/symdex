const SECRET_ASSIGNMENT_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "access_key",
    "secret_key",
    "client_secret",
    "auth_token",
    "access_token",
    "refresh_token",
    "bearer_token",
    "password",
];

const TOKEN_PREFIXES: &[&str] = &["ghp_", "github_pat_", "xoxb-", "sk-"];

pub fn secret_exclusion_reason(relative_path: &str, text: &str) -> Option<&'static str> {
    if is_env_path(relative_path) {
        return Some("likely_env_file");
    }
    if text.contains("-----BEGIN ") && text.contains("PRIVATE KEY-----") {
        return Some("likely_private_key");
    }
    if contains_connection_string(text) {
        return Some("likely_connection_string");
    }
    if contains_token_prefix(text) {
        return Some("likely_access_token");
    }
    if contains_secret_assignment(text) {
        return Some("likely_credential_assignment");
    }
    None
}

fn is_env_path(relative_path: &str) -> bool {
    relative_path
        .rsplit('/')
        .next()
        .is_some_and(|name| name == ".env" || name.starts_with(".env."))
}

fn contains_connection_string(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["postgres://", "postgresql://", "mysql://", "mongodb://"]
        .iter()
        .any(|scheme| {
            lower
                .find(scheme)
                .and_then(|start| lower[start..].find('@'))
                .is_some()
        })
}

fn contains_token_prefix(text: &str) -> bool {
    TOKEN_PREFIXES.iter().any(|prefix| {
        text.find(prefix).is_some_and(|start| {
            let tail = &text[start + prefix.len()..];
            tail.chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
                .count()
                >= 16
        })
    })
}

fn contains_secret_assignment(text: &str) -> bool {
    text.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        SECRET_ASSIGNMENT_KEYS.iter().any(|key| {
            lower.find(key).is_some_and(|start| {
                let after_key = &line[start + key.len()..];
                let Some(separator) = after_key.find(['=', ':']) else {
                    return false;
                };
                has_secret_like_value(&after_key[separator + 1..])
            })
        })
    })
}

fn has_secret_like_value(value: &str) -> bool {
    let trimmed = value
        .trim()
        .trim_matches(',')
        .trim_matches(';')
        .trim_matches('"')
        .trim_matches('\'');
    trimmed.len() >= 16
        && trimmed
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .count()
            >= 12
}

#[cfg(test)]
mod tests {
    use crate::secret_exclusion_reason;

    #[test]
    fn detects_private_key_material() {
        let text =
            "const KEY: &str = \"-----BEGIN PRIVATE KEY-----\\nabc\\n-----END PRIVATE KEY-----\";";

        assert_eq!(
            secret_exclusion_reason("src/lib.rs", text),
            Some("likely_private_key")
        );
    }

    #[test]
    fn detects_credential_assignments() {
        let text = r#"let api_key = "abcdefghijklmnopqrstuvwxyz123456";"#;

        assert_eq!(
            secret_exclusion_reason("src/lib.rs", text),
            Some("likely_credential_assignment")
        );
    }

    #[test]
    fn ignores_short_placeholder_assignments() {
        let text = r#"let api_key = "test";"#;

        assert_eq!(secret_exclusion_reason("src/lib.rs", text), None);
    }

    #[test]
    fn detects_connection_strings_with_credentials() {
        let text = r#"let url = "postgres://user:password@localhost/db";"#;

        assert_eq!(
            secret_exclusion_reason("src/lib.rs", text),
            Some("likely_connection_string")
        );
    }
}
