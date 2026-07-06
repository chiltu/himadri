use regex::Regex;

pub struct Redactor {
    email_regex: Regex,
    jwt_regex: Regex,
    aws_key_regex: Regex,
    api_key_regex: Regex,
    bearer_regex: Regex,
}

impl Redactor {
    pub fn new() -> Self {
        Self {
            email_regex: Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}").unwrap(),
            jwt_regex: Regex::new(r"eyJ[a-zA-Z0-9_-]+\.eyJ[a-zA-Z0-9_-]+\.[a-zA-Z0-9_-]+").unwrap(),
            aws_key_regex: Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
            // OpenAI-style secrets — including this gateway's own `sk-…`
            // keys — are the most likely credential to appear in prompts.
            api_key_regex: Regex::new(r"sk-[a-zA-Z0-9_-]{16,}").unwrap(),
            bearer_regex: Regex::new(r"(?i)bearer\s+[a-zA-Z0-9._~+/=-]{16,}").unwrap(),
        }
    }

    pub fn redact(&self, input: &str) -> String {
        // Apply each pattern in turn, reallocating only when one actually
        // matches: `replace_all` returns a borrowed `Cow` (no allocation) on
        // a clean segment, so a prompt with no secrets allocates just once
        // (the final `into_owned`).
        let mut result: std::borrow::Cow<str> = std::borrow::Cow::Borrowed(input);
        for (regex, replacement) in [
            (&self.jwt_regex, "[REDACTED_JWT]"),
            (&self.bearer_regex, "[REDACTED_BEARER_TOKEN]"),
            (&self.api_key_regex, "[REDACTED_API_KEY]"),
            (&self.email_regex, "[REDACTED_EMAIL]"),
            (&self.aws_key_regex, "[REDACTED_AWS_KEY]"),
        ] {
            if let std::borrow::Cow::Owned(replaced) = regex.replace_all(&result, replacement) {
                result = std::borrow::Cow::Owned(replaced);
            }
        }
        result.into_owned()
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
