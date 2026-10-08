use std::{fmt, io};

/// The layer an effective setting came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Flag,
    Env,
    File,
    Default,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Flag => "flag",
            Self::Env => "env",
            Self::File => "file",
            Self::Default => "default",
        })
    }
}

/// An effective value and the layer that set it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Setting<T> {
    pub(crate) value: T,
    pub(crate) source: Source,
}

impl<T> Setting<T> {
    pub(crate) fn new(value: T, source: Source) -> Self {
        Self { value, source }
    }
}

/// The highest-precedence layer that sets a value, if any does.
pub(crate) fn pick<T>(flag: Option<T>, env: Option<T>, file: Option<T>) -> Option<Setting<T>> {
    flag.map(|value| Setting::new(value, Source::Flag))
        .or_else(|| env.map(|value| Setting::new(value, Source::Env)))
        .or_else(|| file.map(|value| Setting::new(value, Source::File)))
}

/// Like [`pick`], falling back to the built-in default.
pub(crate) fn pick_or<T>(
    flag: Option<T>,
    env: Option<T>,
    file: Option<T>,
    default: T,
) -> Setting<T> {
    pick(flag, env, file).unwrap_or_else(|| Setting::new(default, Source::Default))
}

/// One line of `vorp config show`: key, value, and where it came from. An
/// unset optional setting has neither value nor source.
pub(crate) type Row = (&'static str, Option<(String, Source)>);

pub(crate) fn row<T: fmt::Display>(key: &'static str, setting: &Setting<T>) -> Row {
    (key, Some((setting.value.to_string(), setting.source)))
}

pub(crate) fn optional_row<T: fmt::Display>(
    key: &'static str,
    setting: &Option<Setting<T>>,
) -> Row {
    match setting {
        Some(setting) => row(key, setting),
        None => (key, None),
    }
}

pub(crate) fn write_rows(out: &mut impl io::Write, rows: &[Row]) -> io::Result<()> {
    let width = rows.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
    for (key, value) in rows {
        match value {
            Some((value, source)) => writeln!(out, "{key:<width$}  {value}  ({source})")?,
            None => writeln!(out, "{key:<width$}  (not set)")?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_prefers_flag_then_env_then_file_then_default() {
        let cases = [
            (Some(1), Some(2), Some(3), Setting::new(1, Source::Flag)),
            (None, Some(2), Some(3), Setting::new(2, Source::Env)),
            (None, None, Some(3), Setting::new(3, Source::File)),
            (None, None, None, Setting::new(4, Source::Default)),
            (Some(1), None, Some(3), Setting::new(1, Source::Flag)),
            (Some(1), Some(2), None, Setting::new(1, Source::Flag)),
            (None, Some(2), None, Setting::new(2, Source::Env)),
        ];
        for (flag, env, file, expected) in cases {
            assert_eq!(
                pick_or(flag, env, file, 4),
                expected,
                "{flag:?} {env:?} {file:?}"
            );
        }
        assert_eq!(pick::<u8>(None, None, None), None);
    }

    #[test]
    fn rows_are_aligned_and_mark_unset_values() {
        let mut out = Vec::new();
        write_rows(
            &mut out,
            &[
                row("listen", &Setting::new("0.0.0.0:443", Source::Default)),
                optional_row::<String>("tls.cert", &None),
            ],
        )
        .expect("write to a Vec cannot fail");
        assert_eq!(
            String::from_utf8(out).expect("rows are UTF-8"),
            "listen    0.0.0.0:443  (default)\ntls.cert  (not set)\n"
        );
    }
}
