/// ssh(1)'s options that take a value.
const OPTIONS_WITH_VALUE: &str = "BbcDEeFIiJLlmOoPpQRSWw";

/// ssh's arguments as ssh reads them, split into what goes before the
/// destination and the remote command.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SplitArguments {
    pub options: Vec<String>,
    pub command: Vec<String>,
    /// From `-l`, applied as the destination's user instead.
    pub login: Option<String>,
}

/// Reads arguments the way ssh reads its own: options first, then the
/// remote command from the first word that is not one (or after `--`).
///
/// Refuses what would take the connection past the device's channel or
/// replace the configuration that pins its host and certificate: `-J`,
/// `-W`, `-F`, and `-o ProxyCommand`/`ProxyJump`. Same rules as
/// sntns-platform's `splitSshArguments`, so both tools refuse alike.
pub(crate) fn split(arguments: &[String]) -> Result<SplitArguments, String> {
    let mut split = SplitArguments::default();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" {
            split.command = arguments[index + 1..].to_vec();
            return Ok(split);
        }
        if argument.len() < 2 || !argument.starts_with('-') {
            split.command = arguments[index..].to_vec();
            return Ok(split);
        }
        // A cluster such as -tt or -vvv, possibly ending in an option that
        // takes a value, attached (-L8080:host:80) or as the next word.
        let letters: Vec<char> = argument[1..].chars().collect();
        for (position, &letter) in letters.iter().enumerate() {
            match letter {
                'J' => return Err("-J would reach the device some other way than its channel".into()),
                'W' => {
                    return Err("-W would forward stdio somewhere other than the device's shell".into())
                }
                'F' => {
                    return Err(
                        "-F would replace the configuration that pins the device's host and certificate"
                            .into(),
                    )
                }
                _ => {}
            }
            if !OPTIONS_WITH_VALUE.contains(letter) {
                split.options.push(format!("-{letter}"));
                continue;
            }
            let attached: String = letters[position + 1..].iter().collect();
            let value = if attached.is_empty() {
                index += 1;
                arguments
                    .get(index)
                    .cloned()
                    .ok_or_else(|| format!("-{letter} needs a value"))?
            } else {
                attached
            };
            match letter {
                'l' => split.login = Some(value),
                'o' => {
                    check_option(&value)?;
                    split.options.extend(["-o".to_owned(), value]);
                }
                _ => split.options.extend([format!("-{letter}"), value]),
            }
            break;
        }
        index += 1;
    }
    Ok(split)
}

/// An `-o` value (`Key=value` or `Key value`) that would bypass the channel.
pub(crate) fn check_option(option: &str) -> Result<(), String> {
    let key = option
        .split(|c: char| c == '=' || c.is_whitespace())
        .next()
        .unwrap_or_default();
    if key.eq_ignore_ascii_case("ProxyCommand") || key.eq_ignore_ascii_case("ProxyJump") {
        return Err(format!(
            "-o {key} would reach the device some other way than its channel"
        ));
    }
    Ok(())
}

/// scp(1)'s options that take a value (`-l` is a bandwidth limit here, not
/// a login).
const SCP_OPTIONS_WITH_VALUE: &str = "cDFiJloPSX";

/// One scp operand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Operand {
    Local(String),
    Remote {
        user: Option<String>,
        host: String,
        path: String,
    },
}

/// scp's arguments, split into options and operands.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ScpArguments {
    pub options: Vec<String>,
    pub operands: Vec<Operand>,
}

/// Reads arguments the way scp reads its own: options, then operands
/// (from the first word that is not an option, or after `--`). An operand
/// is remote when it has a `:` with no `/` before it -- scp's own rule --
/// as `[user@]host:path`.
///
/// Refuses what would bypass the device's channel or the pinning, as for
/// ssh (`-J`, `-F`, `-o ProxyCommand`/`ProxyJump`), plus `-S`, which would
/// replace the ssh program carrying the pinned options altogether.
pub(crate) fn split_scp(arguments: &[String]) -> Result<ScpArguments, String> {
    let mut split = ScpArguments::default();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" {
            index += 1;
            break;
        }
        if argument.len() < 2 || !argument.starts_with('-') {
            break;
        }
        let letters: Vec<char> = argument[1..].chars().collect();
        for (position, &letter) in letters.iter().enumerate() {
            match letter {
                'J' => return Err("-J would reach the device some other way than its channel".into()),
                'F' => {
                    return Err(
                        "-F would replace the configuration that pins the device's host and certificate"
                            .into(),
                    )
                }
                'S' => return Err("-S would replace the ssh program that carries the channel".into()),
                _ => {}
            }
            if !SCP_OPTIONS_WITH_VALUE.contains(letter) {
                split.options.push(format!("-{letter}"));
                continue;
            }
            let attached: String = letters[position + 1..].iter().collect();
            let value = if attached.is_empty() {
                index += 1;
                arguments
                    .get(index)
                    .cloned()
                    .ok_or_else(|| format!("-{letter} needs a value"))?
            } else {
                attached
            };
            if letter == 'o' {
                check_option(&value)?;
            }
            split.options.extend([format!("-{letter}"), value]);
            break;
        }
        index += 1;
    }
    for word in &arguments[index..] {
        split.operands.push(operand(word)?);
    }
    Ok(split)
}

fn operand(word: &str) -> Result<Operand, String> {
    if word.starts_with("scp://") {
        return Err(format!(
            "{word}: scp:// URIs are not supported, write [user@]DEVICE:path"
        ));
    }
    let Some(colon) = word.find(':') else {
        return Ok(Operand::Local(word.to_owned()));
    };
    if colon == 0 || word[..colon].contains('/') {
        return Ok(Operand::Local(word.to_owned()));
    }
    let (user, host) = match word[..colon].rsplit_once('@') {
        Some((user, host)) => (Some(user.to_owned()), host.to_owned()),
        None => (None, word[..colon].to_owned()),
    };
    Ok(Operand::Remote {
        user,
        host,
        path: word[colon + 1..].to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn options_then_command() {
        let split = split(&args(&[
            "-t",
            "-L",
            "8080:127.0.0.1:80",
            "-vv",
            "uptime",
            "-a",
        ]))
        .unwrap();
        assert_eq!(
            split.options,
            args(&["-t", "-L", "8080:127.0.0.1:80", "-v", "-v"])
        );
        assert_eq!(split.command, args(&["uptime", "-a"]));
    }

    #[test]
    fn attached_values_and_double_dash() {
        let split = split(&args(&["-tL8080:h:80", "--", "-x"])).unwrap();
        assert_eq!(split.options, args(&["-t", "-L", "8080:h:80"]));
        assert_eq!(split.command, args(&["-x"]));
    }

    #[test]
    fn login_becomes_the_destination_user() {
        let split = split(&args(&["-l", "root", "id"])).unwrap();
        assert_eq!(split.login.as_deref(), Some("root"));
        assert!(split.options.is_empty());
    }

    #[test]
    fn refuses_what_bypasses_the_channel() {
        for refused in [
            &["-J", "jump"][..],
            &["-W", "h:22"],
            &["-F", "/tmp/config"],
            &["-tJjump"],
            &["-o", "ProxyCommand=nc x 22"],
            &["-oproxyjump jump"],
        ] {
            assert!(split(&args(refused)).is_err(), "{refused:?}");
        }
        assert!(split(&args(&["-o", "ServerAliveInterval=5"])).is_ok());
    }

    #[test]
    fn a_missing_value_is_an_error() {
        assert!(split(&args(&["-L"])).is_err());
    }

    #[test]
    fn scp_options_then_operands() {
        let split = split_scp(&args(&["-rp", "-l", "800", "./dist", "DEV:/data/"])).unwrap();
        assert_eq!(split.options, args(&["-r", "-p", "-l", "800"]));
        assert_eq!(
            split.operands,
            [
                Operand::Local("./dist".into()),
                Operand::Remote {
                    user: None,
                    host: "DEV".into(),
                    path: "/data/".into()
                }
            ]
        );
    }

    #[test]
    fn scp_remote_follows_scps_own_colon_rule() {
        let split = split_scp(&args(&["root@DEV:", "./a:b/c", "/tmp/x:y"])).unwrap();
        assert_eq!(
            split.operands[0],
            Operand::Remote {
                user: Some("root".into()),
                host: "DEV".into(),
                path: String::new()
            }
        );
        assert_eq!(split.operands[1], Operand::Local("./a:b/c".into()));
        assert_eq!(split.operands[2], Operand::Local("/tmp/x:y".into()));
        // `--` ends the options; a file named like one follows it.
        let split = split_scp(&args(&["-r", "--", "-file", "DEV:"])).unwrap();
        assert_eq!(split.options, args(&["-r"]));
        assert_eq!(split.operands[0], Operand::Local("-file".into()));
    }

    #[test]
    fn scp_refuses_what_bypasses_the_channel() {
        for refused in [
            &["-J", "jump", "a", "DEV:"][..],
            &["-F", "cfg", "a", "DEV:"],
            &["-S", "/bin/evil", "a", "DEV:"],
            &["-oProxyCommand=nc x 22", "a", "DEV:"],
            &["a", "scp://DEV/x"],
        ] {
            assert!(split_scp(&args(refused)).is_err(), "{refused:?}");
        }
    }
}
