use regex::Regex;

pub struct Redactor {
    email_regex: Regex,
    jwt_regex: Regex,
    aws_key_regex: Regex,
}

impl Redactor {
    pub fn new() -> Self {
        Self {
            email_regex: Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}").unwrap(),
            jwt_regex: Regex::new(r"eyJ[a-zA-Z0-9_-]+\.eyJ[a-zA-Z0-9_-]+\.[a-zA-Z0-9_-]+").unwrap(),
            aws_key_regex: Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
        }
    }

    pub fn redact(&self, input: &str) -> String {
        let mut result = input.to_string();

        result = self
            .email_regex
            .replace_all(&result, "[REDACTED_EMAIL]")
            .to_string();
        result = self
            .jwt_regex
            .replace_all(&result, "[REDACTED_JWT]")
            .to_string();
        result = self
            .aws_key_regex
            .replace_all(&result, "[REDACTED_AWS_KEY]")
            .to_string();

        result
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_email() {
        let redactor = Redactor::new();
        let result = redactor.redact("Contact user@example.com for info");
        assert!(result.contains("[REDACTED_EMAIL]"));
        assert!(!result.contains("user@example.com"));
    }

    #[test]
    fn test_redact_aws_key() {
        let redactor = Redactor::new();
        let result = redactor.redact("Key: AKIAIOSFODNN7EXAMPLE");
        assert!(result.contains("[REDACTED_AWS_KEY]"));
        assert!(!result.contains("AKIAIOSFODNN7EXAMPLE"));
    }
}
