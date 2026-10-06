/// A label map, sorted for stable output.
pub type Labels = std::collections::BTreeMap<String, String>;

/// A device of the account, by its name (the hardware serial).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    /// `None` when not asked for (listing names only).
    pub labels: Option<Labels>,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid label {0:?}: expected key=value")]
pub struct InvalidLabel(pub String);

/// `key=value` pairs, as every `--label`/`--selector` takes them. Here
/// rather than in a transport so that labels select devices the same way
/// wherever they do (`rmra device ls`, `rmra deploy --selector`). The value
/// may be empty or hold `=`; the key may not be empty.
pub fn parse_labels<S: AsRef<str>>(pairs: &[S]) -> Result<Labels, InvalidLabel> {
    pairs
        .iter()
        .map(|pair| {
            let pair = pair.as_ref();
            match pair.split_once('=') {
                Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
                _ => Err(InvalidLabel(pair.to_owned())),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_value_pairs() {
        assert_eq!(
            parse_labels(&["board=rp5", "note=a=b", "empty="]).unwrap(),
            Labels::from([
                ("board".into(), "rp5".into()),
                ("empty".into(), String::new()),
                ("note".into(), "a=b".into()),
            ])
        );
        assert_eq!(parse_labels(&["=rp5"]).unwrap_err().0, "=rp5");
        assert_eq!(parse_labels(&["board"]).unwrap_err().0, "board");
    }
}
