use crate::{utils, Error, Result};
use regex::bytes::Regex;
use std::{borrow::Cow, fs, fs::File, io::prelude::*, path::Path};

pub(crate) struct Replacer {
    regexes: Vec<Regex>,
    replace_withs: Vec<Vec<u8>>,
    is_literal: bool, // -s
    max_replacements: usize,
}

impl Replacer {
    pub(crate) fn new(
        look_for: String,
        replace_with: String,
        is_literal: bool,
        flags: Option<String>,
        replacements: Option<usize>,
        extra: Vec<String>,
    ) -> Result<Self> {
        fn create(
            look_for: String,
            replace_with: String,
            is_literal: bool,
            flags: Option<&str>,
        ) -> Result<(Regex, Vec<u8>)> {
            let (look_for, replace_with) = if is_literal {
                (regex::escape(&look_for), replace_with.into_bytes())
            } else {
                (
                    look_for,
                    utils::unescape(&replace_with)
                        .unwrap_or(replace_with)
                        .into_bytes(),
                )
            };

            let mut regex = regex::bytes::RegexBuilder::new(&look_for);
            regex.multi_line(true);

            if let Some(flags) = flags {
                flags.chars().for_each(|c| {
                    #[rustfmt::skip]
                match c {
                    'c' => { regex.case_insensitive(false); },
                    'i' => { regex.case_insensitive(true); },
                    'm' => {},
                    'e' => { regex.multi_line(false); },
                    's' => {
                        if !flags.contains('m') {
                            regex.multi_line(false);
                        }
                        regex.dot_matches_new_line(true);
                    },
                    'w' => {
                        regex = regex::bytes::RegexBuilder::new(&format!(
                            "\\b{}\\b",
                            look_for
                        ));
                    },
                    _ => {},
                };
                });
            };
            Ok((regex.build()?, replace_with))
        }
        let capacity = extra.len() / 2 + 1;
        let mut regexes = Vec::with_capacity(capacity);
        let mut replace_withs = Vec::with_capacity(capacity);
        let first =
            create(look_for, replace_with, is_literal, flags.as_deref())?;

        regexes.push(first.0);
        replace_withs.push(first.1);

        let mut it = extra.into_iter();
        while let Some(look_for) = it.next() {
            let replace_with = it.next().expect("The extra pattern list doesn't have an even lenght");

            let (regex, replace_with) =
                create(look_for, replace_with, is_literal, flags.as_deref())?;
            regexes.push(regex);
            replace_withs.push(replace_with);
        }

        Ok(Self {
            regexes,
            replace_withs,
            is_literal,
            max_replacements: replacements.unwrap_or(0),
        })
    }

    pub(crate) fn has_matches(&self, content: &[u8]) -> bool {
        self.regexes.iter().any(|r| r.is_match(content))
    }

    pub(crate) fn check_not_empty(mut file: File) -> Result<()> {
        let mut buf: [u8; 1] = Default::default();
        file.read_exact(&mut buf)?;
        Ok(())
    }

    fn generic_replace<'a>(
        &self,
        content: &'a [u8],
        replace: impl Iterator<Item = impl regex::bytes::Replacer>,
    ) -> Cow<'a, [u8]> {
        let mut result = Cow::Borrowed(content);
        for (regex, replace_with) in self.regexes.iter().zip(replace) {
            result = Cow::Owned(
                regex
                    .replacen(&result, self.max_replacements, replace_with)
                    .into_owned(),
            );
        }
        result
    }

    pub(crate) fn replace<'a>(&'a self, content: &'a [u8]) -> Cow<'a, [u8]> {
        if self.is_literal {
            let rep =
                self.replace_withs.iter().map(|r| regex::bytes::NoExpand(r));
            self.generic_replace(content, rep)
        } else {
            self.generic_replace(content, self.replace_withs.iter())
        }
    }

    pub(crate) fn replace_preview<'a>(
        &'a self,
        content: &'a [u8],
    ) -> Cow<'a, [u8]> {
        if self.regexes.len() > 1 {
            return self.replace(content);
        }
        let regex = &self.regexes[0];
        let replace = &self.replace_withs[0];
        let mut v = Vec::<u8>::new();
        let mut captures = regex.captures_iter(content);

        for sur_text in regex.split(content) {
            use regex::bytes::Replacer;

            v.extend(sur_text);
            if let Some(capture) = captures.next() {
                v.extend_from_slice(
                    ansi_term::Color::Green.prefix().to_string().as_bytes(),
                );
                if self.is_literal {
                    regex::bytes::NoExpand(&replace)
                        .replace_append(&capture, &mut v);
                } else {
                    (&*replace).replace_append(&capture, &mut v);
                }
                v.extend_from_slice(
                    ansi_term::Color::Green.suffix().to_string().as_bytes(),
                );
            }
        }

        return Cow::Owned(v);
    }

    pub(crate) fn replace_file(&self, path: &Path) -> Result<()> {
        use memmap2::{Mmap, MmapMut};
        use std::ops::DerefMut;

        if Self::check_not_empty(File::open(path)?).is_err() {
            return Ok(());
        }

        let source = File::open(path)?;
        let meta = fs::metadata(path)?;
        let mmap_source = unsafe { Mmap::map(&source) }?;
        let replaced = self.replace(&mmap_source);

        let target = tempfile::NamedTempFile::new_in(
            path.parent()
                .ok_or_else(|| Error::InvalidPath(path.to_path_buf()))?,
        )?;
        let file = target.as_file();
        file.set_len(replaced.len() as u64)?;
        file.set_permissions(meta.permissions())?;

        if !replaced.is_empty() {
            let mut mmap_target = unsafe { MmapMut::map_mut(file) }?;
            mmap_target.deref_mut().write_all(&replaced)?;
            mmap_target.flush_async()?;
        }

        drop(mmap_source);
        drop(source);

        target.persist(fs::canonicalize(path)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replace(
        look_for: impl Into<String>,
        replace_with: impl Into<String>,
        literal: bool,
        flags: Option<&'static str>,
        src: &'static str,
        target: &'static str,
    ) {
        let replacer = Replacer::new(
            look_for.into(),
            replace_with.into(),
            literal,
            flags.map(ToOwned::to_owned),
            None,
            vec![],
        )
        .unwrap();
        assert_eq!(
            std::str::from_utf8(&replacer.replace(src.as_bytes())),
            Ok(target)
        );
    }

    #[test]
    fn default_global() {
        replace("a", "b", false, None, "aaa", "bbb");
    }

    #[test]
    fn escaped_char_preservation() {
        replace("a", "b", false, None, "a\\n", "b\\n");
    }

    #[test]
    fn case_sensitive_default() {
        replace("abc", "x", false, None, "abcABC", "xABC");
        replace("abc", "x", true, None, "abcABC", "xABC");
    }

    #[test]
    fn sanity_check_literal_replacements() {
        replace("((special[]))", "x", true, None, "((special[]))y", "xy");
    }

    #[test]
    fn unescape_regex_replacements() {
        replace("test", r"\n", false, None, "testtest", "\n\n");
    }

    #[test]
    fn no_unescape_literal_replacements() {
        replace("test", r"\n", true, None, "testtest", r"\n\n");
    }

    #[test]
    fn full_word_replace() {
        replace("abc", "def", false, Some("w"), "abcd abc", "abcd def");
    }
}
