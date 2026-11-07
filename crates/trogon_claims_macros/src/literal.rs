use crate::constants::{ESCAPED, NOT_A_STRING};

pub(crate) fn string_value(text: &str) -> Result<String, &'static str> {
    if let Some(raw) = text.strip_prefix('r') {
        let hashes = raw.bytes().take_while(|byte| *byte == b'#').count();
        let body = &raw[hashes..];
        let closing = format!("\"{}", "#".repeat(hashes));
        let inner = body
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix(closing.as_str()))
            .ok_or(NOT_A_STRING)?;
        return Ok(inner.to_owned());
    }
    let inner = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or(NOT_A_STRING)?;
    if inner.contains('\\') {
        return Err(ESCAPED);
    }
    Ok(inner.to_owned())
}

#[cfg(test)]
mod tests;
