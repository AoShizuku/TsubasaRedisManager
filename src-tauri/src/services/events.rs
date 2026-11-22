use crate::models::RedisCommandArg;

pub fn summarize_args(command: &str, args: &[RedisCommandArg]) -> String {
    let upper = command.trim().to_ascii_uppercase();
    if upper == "AUTH" {
        return "***".into();
    }

    let sensitive_positions = sensitive_positions(&upper, args);
    args.iter()
        .enumerate()
        .map(|(index, arg)| summarize_arg(&upper, args, index, arg, &sensitive_positions))
        .collect::<Vec<_>>()
        .join(" ")
}

fn summarize_arg(
    command: &str,
    args: &[RedisCommandArg],
    index: usize,
    arg: &RedisCommandArg,
    sensitive_positions: &[bool],
) -> String {
    if sensitive_positions.get(index).copied().unwrap_or(false) {
        return "***".into();
    }

    match arg {
        RedisCommandArg::String { value } => {
            if command == "ACL" && is_acl_setuser(args) {
                summarize_acl_setuser_token(value)
            } else {
                summarize_string(value)
            }
        }
        RedisCommandArg::Binary { base64 } => format!("<binary:{}>", base64.len()),
    }
}

fn sensitive_positions(command: &str, args: &[RedisCommandArg]) -> Vec<bool> {
    let mut positions = vec![false; args.len()];
    match command {
        "HELLO" => mark_hello_auth(args, &mut positions),
        "CONFIG" => mark_config_set_password(args, &mut positions),
        "MIGRATE" => mark_migrate_auth(args, &mut positions),
        _ => {}
    }
    positions
}

fn mark_hello_auth(args: &[RedisCommandArg], positions: &mut [bool]) {
    for index in 0..args.len() {
        if string_arg_eq(args, index, "AUTH") && index + 2 < args.len() {
            positions[index + 2] = true;
        }
    }
}

fn mark_config_set_password(args: &[RedisCommandArg], positions: &mut [bool]) {
    if args.len() >= 3
        && string_arg_eq(args, 0, "SET")
        && string_arg(args, 1).is_some_and(is_password_config_name)
    {
        positions[2] = true;
    }
}

fn mark_migrate_auth(args: &[RedisCommandArg], positions: &mut [bool]) {
    for index in 0..args.len() {
        if string_arg_eq(args, index, "AUTH") && index + 1 < args.len() {
            positions[index + 1] = true;
        } else if string_arg_eq(args, index, "AUTH2") && index + 2 < args.len() {
            positions[index + 2] = true;
        }
    }
}

fn is_acl_setuser(args: &[RedisCommandArg]) -> bool {
    string_arg_eq(args, 0, "SETUSER")
}

fn summarize_acl_setuser_token(value: &str) -> String {
    if value.starts_with('>') {
        ">***".into()
    } else if value.starts_with('<') {
        "<***".into()
    } else if value.starts_with('#') {
        "#***".into()
    } else {
        summarize_string(value)
    }
}

fn summarize_string(value: &str) -> String {
    if value.len() > 100 {
        let mut end = 100;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &value[..end])
    } else {
        value.to_string()
    }
}

fn string_arg(args: &[RedisCommandArg], index: usize) -> Option<&str> {
    match args.get(index) {
        Some(RedisCommandArg::String { value }) => Some(value),
        Some(RedisCommandArg::Binary { .. }) | None => None,
    }
}

fn string_arg_eq(args: &[RedisCommandArg], index: usize, expected: &str) -> bool {
    string_arg(args, index).is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

fn is_password_config_name(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    lower.contains("pass") || lower.contains("auth")
}

#[cfg(test)]
mod tests {
    use super::summarize_args;
    use crate::models::RedisCommandArg;

    #[test]
    fn masks_auth_arguments() {
        let args = vec![RedisCommandArg::String {
            value: "secret".into(),
        }];
        assert_eq!(summarize_args("AUTH", &args), "***");
    }

    #[test]
    fn masks_auth_arguments_case_and_whitespace_insensitively() {
        let args = vec![RedisCommandArg::String {
            value: "secret".into(),
        }];
        assert_eq!(summarize_args("  aUtH  ", &args), "***");
    }

    #[test]
    fn masks_hello_auth_password() {
        let args = vec![
            RedisCommandArg::String { value: "3".into() },
            RedisCommandArg::String {
                value: "AUTH".into(),
            },
            RedisCommandArg::String {
                value: "default".into(),
            },
            RedisCommandArg::String {
                value: "secret".into(),
            },
        ];
        assert_eq!(summarize_args("HELLO", &args), "3 AUTH default ***");
    }

    #[test]
    fn masks_acl_setuser_password_tokens() {
        let args = vec![
            RedisCommandArg::String {
                value: "SETUSER".into(),
            },
            RedisCommandArg::String {
                value: "alice".into(),
            },
            RedisCommandArg::String {
                value: ">secret".into(),
            },
            RedisCommandArg::String {
                value: "<old-secret".into(),
            },
            RedisCommandArg::String {
                value: "#hash".into(),
            },
            RedisCommandArg::String {
                value: "+get".into(),
            },
        ];
        assert_eq!(
            summarize_args("ACL", &args),
            "SETUSER alice >*** <*** #*** +get"
        );
    }

    #[test]
    fn masks_config_password_values() {
        let requirepass = vec![
            RedisCommandArg::String {
                value: "SET".into(),
            },
            RedisCommandArg::String {
                value: "requirepass".into(),
            },
            RedisCommandArg::String {
                value: "secret".into(),
            },
        ];
        let masterauth = vec![
            RedisCommandArg::String {
                value: "SET".into(),
            },
            RedisCommandArg::String {
                value: "masterauth".into(),
            },
            RedisCommandArg::String {
                value: "replica-secret".into(),
            },
        ];

        assert_eq!(
            summarize_args("CONFIG", &requirepass),
            "SET requirepass ***"
        );
        assert_eq!(summarize_args("CONFIG", &masterauth), "SET masterauth ***");
    }

    #[test]
    fn masks_migrate_auth_passwords() {
        let auth = vec![
            RedisCommandArg::String {
                value: "127.0.0.1".into(),
            },
            RedisCommandArg::String {
                value: "6379".into(),
            },
            RedisCommandArg::String { value: "k".into() },
            RedisCommandArg::String { value: "0".into() },
            RedisCommandArg::String {
                value: "5000".into(),
            },
            RedisCommandArg::String {
                value: "AUTH".into(),
            },
            RedisCommandArg::String {
                value: "secret".into(),
            },
        ];
        let auth2 = vec![
            RedisCommandArg::String {
                value: "127.0.0.1".into(),
            },
            RedisCommandArg::String {
                value: "6379".into(),
            },
            RedisCommandArg::String { value: "k".into() },
            RedisCommandArg::String { value: "0".into() },
            RedisCommandArg::String {
                value: "5000".into(),
            },
            RedisCommandArg::String {
                value: "AUTH2".into(),
            },
            RedisCommandArg::String {
                value: "default".into(),
            },
            RedisCommandArg::String {
                value: "secret".into(),
            },
        ];

        assert_eq!(
            summarize_args("MIGRATE", &auth),
            "127.0.0.1 6379 k 0 5000 AUTH ***"
        );
        assert_eq!(
            summarize_args("MIGRATE", &auth2),
            "127.0.0.1 6379 k 0 5000 AUTH2 default ***"
        );
    }

    #[test]
    fn summarizes_binary_arguments_without_payload() {
        let args = vec![RedisCommandArg::Binary {
            base64: "AAECAw==".into(),
        }];
        assert_eq!(summarize_args("RESTORE", &args), "<binary:8>");
    }

    #[test]
    fn truncates_long_utf8_string_arguments_without_panicking() {
        let args = vec![RedisCommandArg::String {
            value: "中".repeat(34),
        }];
        assert_eq!(
            summarize_args("SET", &args),
            format!("{}...", "中".repeat(33))
        );
    }
}
