//! `--jq`: what a command prints as JSON, through a jq filter first.

use error_stack::Report;
use jaq_core::{
    data, load,
    load::{Arena, File, Loader},
    unwrap_valr, Compiler, Ctx, Vars,
};
use jaq_json::{read, Val};
use serde_json::Value;

use crate::error::{Error, Result};

/// What `filter` (jq's language: `.[].document.cpu`, `map(.seq)`,
/// `.[] | select(...)`) makes of `input`, one line or more per result: a
/// string as it is, anything else as JSON.
pub(crate) fn apply(filter: &str, input: &Value) -> Result<Vec<String>> {
    let invalid = |why: String| {
        Report::new(Error::Monitor(format!("{filter:?} is not a jq filter"))).attach(why)
    };
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs())
        .chain(formats());
    let funs = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());
    let arena = Arena::default();
    let modules = Loader::new(defs)
        .load(
            &arena,
            File {
                code: filter,
                path: (),
            },
        )
        .map_err(|errors| invalid(unreadable(errors.into_iter().map(|(_, error)| error))))?;
    let compiled = Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errors| {
            let undefined: Vec<_> = errors
                .into_iter()
                .flat_map(|(_, errors)| errors)
                .map(|(name, _)| format!("`{name}` is not defined"))
                .collect();
            invalid(undefined.join("; "))
        })?;

    let input = read::parse_single(input.to_string().as_bytes())
        .map_err(|error| Report::new(Error::Monitor(format!("unreadable JSON: {error}"))))?;
    let context = Ctx::<data::JustLut<Val>>::new(&compiled.lut, Vars::new([]));
    compiled
        .id
        .run((context, input))
        .map(unwrap_valr)
        .map(|result| match result {
            Ok(Val::Null) => Ok("null".to_owned()),
            Ok(value) => Ok(printed(&value)),
            Err(error) => Err(Report::new(Error::Monitor(format!(
                "the jq filter {filter:?} failed: {error}"
            )))),
        })
        .collect()
}

/// `@csv` and `@tsv`, as jq has them.
fn formats() -> impl Iterator<Item = load::parse::Def<&'static str>> {
    load::parse(include_str!("formats.jq"), |parser| parser.defs())
        .expect("formats.jq is jq")
        .into_iter()
}

/// Why a filter could not be read: what was expected, and where.
fn unreadable<'a>(errors: impl Iterator<Item = load::Error<&'a str>>) -> String {
    let at = |found: &str| match found {
        "" => "at its end".to_owned(),
        found => format!("at {found:?}"),
    };
    errors
        .flat_map(|error| match error {
            load::Error::Io(errors) => errors
                .into_iter()
                .map(|(path, why)| format!("{path}: {why}"))
                .collect::<Vec<_>>(),
            load::Error::Lex(errors) => errors
                .into_iter()
                .map(|(expected, found)| format!("expected a {} {}", expected.as_str(), at(found)))
                .collect(),
            load::Error::Parse(errors) => errors
                .into_iter()
                .map(|(expected, found)| format!("expected a {} {}", expected.as_str(), at(found)))
                .collect(),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A result as its line(s): a string without its quotes, for a script to
/// take as it is; the rest as indented JSON.
fn printed(value: &Val) -> String {
    let compact = value.to_string();
    match serde_json::from_str::<Value>(&compact) {
        Ok(Value::String(text)) => text,
        Ok(json) => serde_json::to_string_pretty(&json).unwrap_or(compact),
        // What JSON has no word for (NaN, raw bytes): as jq writes it.
        Err(_) => compact,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_filter_picks_and_reshapes_and_strings_come_bare() {
        let input = json!([
            {"seq": 1, "document": {"state": "ok", "cpu": {"usage-percent": 12.5}}},
            {"seq": 2, "document": {"state": "degraded", "cpu": {"usage-percent": 80}}},
        ]);
        assert_eq!(
            apply(".[].document.state", &input).unwrap(),
            ["ok", "degraded"]
        );
        assert_eq!(
            apply(
                r#".[] | select(.document.cpu."usage-percent" > 50) | .seq"#,
                &input
            )
            .unwrap(),
            ["2"]
        );
        assert_eq!(apply("length", &input).unwrap(), ["2"]);
        assert_eq!(apply(".[0].missing", &input).unwrap(), ["null"]);
        assert_eq!(apply("map(.seq)", &input).unwrap(), ["[\n  1,\n  2\n]"]);
        assert_eq!(
            apply(r#".[0] | "\(.seq): \(.document.state)""#, &input).unwrap(),
            ["1: ok"]
        );
    }

    #[test]
    fn rows_come_as_csv_and_tsv() {
        let input = json!([
            {"name": "eth0", "rx": 12, "up": true, "note": null},
            {"name": "say \"hi\", you", "rx": 1.5, "up": false, "note": "a\tb\\c\nd"},
        ]);
        let rows = ".[] | [.name, .rx, .up, .note]";
        assert_eq!(
            apply(&format!("{rows} | @csv"), &input).unwrap(),
            [
                "\"eth0\",12,true,",
                "\"say \"\"hi\"\", you\",1.5,false,\"a\tb\\c\nd\""
            ]
        );
        assert_eq!(
            apply(&format!("{rows} | @tsv"), &input).unwrap(),
            [
                "eth0\t12\ttrue\t",
                "say \"hi\", you\t1.5\tfalse\ta\\tb\\\\c\\nd"
            ]
        );
        // In a string, the format is applied to what is interpolated.
        assert_eq!(
            apply(r#".[0] | @csv "\([.name, .rx])""#, &input).unwrap(),
            ["\"eth0\",12"]
        );
        let error = apply(".[] | [.] | @csv", &input).unwrap_err();
        assert!(
            error.to_string().contains("not valid in a csv row"),
            "{error}"
        );
    }

    #[test]
    fn a_wrong_filter_and_a_failed_one_say_so() {
        let input = json!({"a": 1});
        let error = apply(".[", &input).unwrap_err();
        assert!(error.to_string().contains("is not a jq filter"), "{error}");
        let error = apply("nothing_like_this", &input).unwrap_err();
        assert!(error.to_string().contains("is not a jq filter"), "{error}");
        let error = apply(".a | keys", &input).unwrap_err();
        assert!(error.to_string().contains("failed"), "{error}");
    }
}
