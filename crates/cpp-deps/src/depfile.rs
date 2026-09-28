//! Decode compiler Make depfiles without shell interpretation or lossy paths.

use alloc::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

/// An undecoded compiler dependency word.
#[derive(Default)]
#[repr(transparent)]
struct Word(Vec<u8>);

impl TryFrom<Word> for OsString
{
    type Error = io::Error;

    #[inline]
    fn try_from(word: Word) -> Result<Self, Self::Error>
    {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            Ok(Self::from_vec(word.0))
        }
        #[cfg(not(unix))]
        {
            let text = String::from_utf8(word.0)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            Ok(Self::from(text))
        }
    }
}

/// Compiler-produced dependency text.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Input<'source>(pub &'source [u8]);

/// Target/dependency distinction, reset at each logical line.
#[derive(Clone, Copy)]
enum Side
{
    /// Discard make targets before the separator.
    Targets,
    /// Collect prerequisites after the separator.
    Dependencies,
}

/// Transfer a completed dependency word to a sorted path set.
///
/// # Specification
/// - ensures: targets are discarded; relative prerequisites retain lexical
///   identity beneath the invocation directory; duplicate prerequisites merge.
/// - fails: a platform cannot represent a prerequisite path.
/// - panics: none.
fn finish(
    word: &mut Word,
    side: Side,
    cwd: &Path,
    paths: &mut BTreeSet<PathBuf>,
) -> io::Result<()>
{
    if word.0.is_empty() {
        return Ok(());
    }
    match side {
        | Side::Targets => word.0.clear(),
        | Side::Dependencies => {
            let word = core::mem::take(word);
            let path = OsString::try_from(word)?;
            paths.insert(cwd.join(path));
        },
    }
    Ok(())
}

/// Decode prerequisites across escaped names, continuations and phony rules.
///
/// # Specification
/// - ensures: Make escapes and doubled dollars decode once, not as shell
///   syntax; target words never become input dependencies.
/// - fails: dangling escapes or text without any target delimiter.
/// - panics: none.
///
/// # Errors
/// Returns invalid-data errors for malformed dependency text.
///
/// # Adequacy
/// - hypothesis: escaped spaces, hashes, dollars, colons and continuations
///   distinguish prerequisites from target and comment text.
/// - witness: `tests::escaped_paths_and_phony_targets`.
pub fn parse(
    input: Input<'_>,
    cwd: &Path,
) -> io::Result<Vec<PathBuf>>
{
    let mut bytes = input.0.iter().copied().peekable();
    let mut paths = BTreeSet::new();
    let mut word = Word::default();
    let mut side = Side::Targets;
    let mut delimited = false;
    while let Some(byte) = bytes.next() {
        match byte {
            | b'\\' => {
                let escaped = bytes.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "dangling depfile escape")
                })?;
                if escaped == b'\n' {
                    finish(&mut word, side, cwd, &mut paths)?;
                }
                else if escaped == b'\r' && bytes.peek() == Some(&b'\n') {
                    bytes.next();
                    finish(&mut word, side, cwd, &mut paths)?;
                }
                else {
                    if !matches!(escaped, b' ' | b'\t' | b'#' | b':' | b'\\') {
                        word.0.push(b'\\');
                    }
                    word.0.push(escaped);
                }
            },
            | b'$' if bytes.peek() == Some(&b'$') => {
                bytes.next();
                word.0.push(b'$');
            },
            | b':' if matches!(side, Side::Targets) => {
                // Drive letters in Windows targets are not the rule delimiter.
                if word.0.len() == 1
                    && word.0.first().is_some_and(u8::is_ascii_alphabetic)
                    && matches!(bytes.peek(), Some(b'/' | b'\\'))
                {
                    word.0.push(byte);
                }
                else {
                    word.0.clear();
                    side = Side::Dependencies;
                    delimited = true;
                }
            },
            | b'#' => {
                finish(&mut word, side, cwd, &mut paths)?;
                for next in bytes.by_ref() {
                    if next == b'\n' {
                        break;
                    }
                }
                side = Side::Targets;
            },
            | b'\n' => {
                finish(&mut word, side, cwd, &mut paths)?;
                side = Side::Targets;
            },
            | byte if byte.is_ascii_whitespace() => finish(&mut word, side, cwd, &mut paths)?,
            | byte => word.0.push(byte),
        }
    }
    finish(&mut word, side, cwd, &mut paths)?;
    if !delimited {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "depfile has no target delimiter",
        ));
    }
    Ok(paths.into_iter().collect())
}

/// Read the complete compiler dependency witness.
///
/// # Specification
/// - ensures: paths resolve from the invocation directory, then canonicalize
///   and deduplicate wrapper-rewritten aliases of the same existing input.
/// - fails: read, syntax, or input resolution errors.
/// - panics: none.
///
/// # Errors
/// Returns the original I/O or dependency syntax error.
pub fn read(
    path: &Path,
    cwd: &Path,
) -> io::Result<Vec<PathBuf>>
{
    let bytes = fs::read(path)?;
    let mut paths = parse(Input(&bytes), cwd)?;
    for path in &mut paths {
        *path = fs::canonicalize(&*path)?;
    }
    paths.sort_unstable();
    paths.dedup();
    Ok(paths)
}

#[cfg(test)]
mod tests
{
    use std::path::Path;
    use std::path::PathBuf;

    use super::Input;
    use super::parse;

    #[test]
    fn escaped_paths_and_phony_targets()
    {
        let text = b"obj\\ name.o another.o: src.cpp include/a\\ b.hpp \\\n hash\\#name.hpp cash$$name.hpp colon\\:name.hpp # ignored.hpp\ninclude/a\\ b.hpp:\n";
        let actual = parse(Input(text), Path::new("/work")).expect("valid compiler depfile");
        let expected = [
            "cash$name.hpp",
            "colon:name.hpp",
            "hash#name.hpp",
            "include/a b.hpp",
            "src.cpp",
        ]
        .map(|name| PathBuf::from("/work").join(name));
        assert_eq!(actual, expected);
        assert!(parse(Input(b"object: trailing\\"), Path::new("/work")).is_err());
        assert!(parse(Input(b"not a rule"), Path::new("/work")).is_err());
    }
}
