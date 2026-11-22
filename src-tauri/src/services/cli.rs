use crate::error::{AppError, AppResult};

pub fn parse_cli(command_line: &str) -> AppResult<Vec<String>> {
    let args =
        shell_words::split(command_line).map_err(|e| AppError::InvalidCommand(e.to_string()))?;
    if args.is_empty() {
        return Err(AppError::InvalidCommand("empty command".into()));
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::parse_cli;

    #[test]
    fn parses_quoted_values() {
        let parsed = parse_cli(r#"set user:1 "hello world""#).unwrap();
        assert_eq!(parsed, vec!["set", "user:1", "hello world"]);
    }

    #[test]
    fn rejects_empty_command() {
        let err = parse_cli("   ").unwrap_err();
        assert_eq!(err.payload().code, "redis.invalid_command");
    }
}
