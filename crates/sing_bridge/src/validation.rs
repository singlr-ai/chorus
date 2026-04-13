use crate::error::SingBridgeError;

const MAX_PROJECT_NAME_LEN: usize = 63;
const MAX_SPEC_ID_LEN: usize = 80;
const MAX_SERVICE_NAME_LEN: usize = 63;

pub fn project_name(value: &str) -> Result<(), SingBridgeError> {
    validate_project_or_spec("project", value, MAX_PROJECT_NAME_LEN)
}

pub fn spec_id(value: &str) -> Result<(), SingBridgeError> {
    validate_project_or_spec("spec_id", value, MAX_SPEC_ID_LEN)
}

pub fn service_name(value: &str) -> Result<(), SingBridgeError> {
    if value.is_empty()
        || value.len() > MAX_SERVICE_NAME_LEN
        || !value
            .chars()
            .enumerate()
            .all(|(index, ch)| service_char_allowed(index == 0, ch))
    {
        return Err(SingBridgeError::invalid_input(
            "service",
            format!(
                "must match [a-zA-Z0-9][a-zA-Z0-9._-]* and be at most {MAX_SERVICE_NAME_LEN} characters"
            ),
        ));
    }

    Ok(())
}

pub fn git_ref(value: &str) -> Result<(), SingBridgeError> {
    if value.is_empty()
        || value.contains("..")
        || !value
            .chars()
            .enumerate()
            .all(|(index, ch)| git_ref_char_allowed(index == 0, ch))
    {
        return Err(SingBridgeError::invalid_input(
            "branch",
            "must match [a-zA-Z0-9][a-zA-Z0-9._/-]* and not contain '..'",
        ));
    }

    Ok(())
}

pub fn title(value: &str) -> Result<String, SingBridgeError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(SingBridgeError::invalid_input("title", "must not be blank"));
    }

    if trimmed.contains('\0') {
        return Err(SingBridgeError::invalid_input(
            "title",
            "must not contain NUL bytes",
        ));
    }

    Ok(trimmed.to_string())
}

fn validate_project_or_spec(
    field: &'static str,
    value: &str,
    max_len: usize,
) -> Result<(), SingBridgeError> {
    if value.is_empty()
        || value.len() > max_len
        || !value
            .chars()
            .enumerate()
            .all(|(index, ch)| project_or_spec_char_allowed(index == 0, ch))
    {
        return Err(SingBridgeError::invalid_input(
            field,
            format!("must match [a-z0-9][a-z0-9-]* and be at most {max_len} characters"),
        ));
    }

    Ok(())
}

fn project_or_spec_char_allowed(first: bool, ch: char) -> bool {
    if first {
        ch.is_ascii_lowercase() || ch.is_ascii_digit()
    } else {
        ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'
    }
}

fn service_char_allowed(first: bool, ch: char) -> bool {
    if first {
        ch.is_ascii_alphanumeric()
    } else {
        ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')
    }
}

fn git_ref_char_allowed(first: bool, ch: char) -> bool {
    if first {
        ch.is_ascii_alphanumeric()
    } else {
        ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '/' | '-')
    }
}
