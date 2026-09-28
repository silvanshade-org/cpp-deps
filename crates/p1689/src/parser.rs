//! Schema-directed JSON parsing with borrowed strings and bounded call depth.

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use alloc::borrow::Cow;
use alloc::vec::Vec;
use core::fmt;

#[cfg(not(all(
    target_arch = "x86_64",
    any(target_feature = "avx2", target_feature = "sse2")
)))]
use memchr::arch::all::memchr::Two as StringFinder;
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
use memchr::arch::x86_64::avx2::memchr::Two as StringFinder;
#[cfg(all(
    target_arch = "x86_64",
    not(target_feature = "avx2"),
    target_feature = "sse2"
))]
use memchr::arch::x86_64::sse2::memchr::Two as StringFinder;

use crate::r5::DepFile;
use crate::r5::DepInfo;
use crate::r5::IsInterface;
use crate::r5::LookupMethod;
use crate::r5::ModuleDesc;
use crate::r5::ModuleName;
use crate::r5::ProvidedModuleDesc;
use crate::r5::RequiredModuleDesc;
use crate::r5::Revision;
use crate::r5::Version;

/// Caller-owned scanner JSON bytes; no UTF-8 validation.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct JsonInput<'source>(pub &'source [u8]);

/// Named failure at the JSON boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseErrorKind
{
    /// Unexpected token, punctuation, or trailing input.
    Syntax,
    /// String contains an invalid escape or surrogate.
    String,
    /// Number is not an unsigned 32-bit JSON integer.
    Integer,
    /// A field appears more than once.
    DuplicateField,
    /// A field is outside the revision 5 schema.
    UnknownField,
    /// A mandatory field is absent.
    MissingField,
    /// Lookup method is not a revision 5 method.
    LookupMethod,
}

/// JSON failure with its byte offset in the original input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseError
{
    /// Byte offset of the rejected input.
    pub offset: usize,
    /// Failure classification.
    pub kind: ParseErrorKind,
}

impl fmt::Display for ParseError
{
    /// Render a bounded diagnostic without copying input.
    ///
    /// # Specification
    /// trivial.
    #[inline]
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result
    {
        {
            let kind = match self.kind {
                | ParseErrorKind::Syntax => "invalid syntax",
                | ParseErrorKind::String => "invalid string",
                | ParseErrorKind::Integer => "invalid integer",
                | ParseErrorKind::DuplicateField => "duplicate field",
                | ParseErrorKind::UnknownField => "unknown field",
                | ParseErrorKind::MissingField => "missing field",
                | ParseErrorKind::LookupMethod => "invalid lookup method",
            };
            write!(f, "P1689 {kind} at byte {}", self.offset)
        }
    }
}
impl core::error::Error for ParseError
{
}

/// Byte cursor and reusable string-search state.
struct Parser<'source>
{
    /// Unconsumed scanner bytes.
    rest: &'source [u8],
    /// Input length for diagnostic offsets.
    length: usize,
    /// Quote/backslash finder initialized once per document.
    finder: StringFinder,
    /// Reused Unicode escape encoding storage.
    utf8: [u8; 4],
}

/// Delimiters with semantic roles in the fixed schema.
#[derive(Clone, Copy)]
enum Delimiter
{
    /// Object members.
    Object,
    /// Array elements.
    Array,
}
/// Container progress and closing delimiter.
struct Sequence
{
    /// Container kind.
    delimiter: Delimiter,
    /// Whether the next member is the first.
    first: bool,
}
/// Presence of another member in a container.
enum Member
{
    /// Parse the next member.
    Next,
    /// Closing punctuation has been consumed.
    End,
}
/// Mask identifying one field inside its owning object.
#[derive(Clone, Copy)]
#[repr(transparent)]
struct Field(u16);
/// Borrowed bytes consumed from input.
#[repr(transparent)]
struct Fragment<'source>(&'source [u8]);

/// Unsigned wire integer.
#[repr(transparent)]
struct Number(u32);
/// Wire Boolean.
#[repr(transparent)]
struct Boolean(bool);
/// Unicode scalar or surrogate code unit.
#[repr(transparent)]
struct CodeUnit(u32);
/// Byte string, borrowed unless unescaping required allocation.
#[repr(transparent)]
struct Text<'source>(Cow<'source, [u8]>);
/// Count of bytes consumed by a token.
#[derive(Clone, Copy)]
#[repr(transparent)]
struct Width(usize);

/// Decode two hexadecimal ASCII bytes per table access.
///
/// # Specification
/// - ensures: valid hexadecimal pairs map to their byte value, independent of
///   letter case; every other pair maps to `u32::MAX`.
/// - panics: none.
///
/// # Adequacy
/// - hypothesis: exhaustive ASCII pairs distinguish case and validity errors.
/// - witness: `hex_pairs`.
static HEX_PAIRS: [u32; 0x1_0000] = {
    let mut table = [u32::MAX; 0x1_0000];
    let mut remaining = table.as_mut_slice();
    let mut index = 0_u32;
    while let Some((entry, tail)) = remaining.split_first_mut() {
        let low = index & 0xff;
        let high = index >> 8_u32;
        let low = match low {
            | 0x30 ..= 0x39 => low.saturating_sub(0x30),
            | 0x41 ..= 0x46 => low.saturating_sub(0x41).saturating_add(10),
            | 0x61 ..= 0x66 => low.saturating_sub(0x61).saturating_add(10),
            | _ => u32::MAX,
        };
        let high = match high {
            | 0x30 ..= 0x39 => high.saturating_sub(0x30),
            | 0x41 ..= 0x46 => high.saturating_sub(0x41).saturating_add(10),
            | 0x61 ..= 0x66 => high.saturating_sub(0x61).saturating_add(10),
            | _ => u32::MAX,
        };
        if low != u32::MAX && high != u32::MAX {
            *entry = (low << 4_u32) | high;
        }
        index = index.saturating_add(1);
        remaining = tail;
    }
    table
};

impl<'source> Parser<'source>
{
    /// Associate a failure with the current byte offset.
    ///
    /// # Specification
    /// trivial.
    fn error(
        &self,
        kind: ParseErrorKind,
    ) -> ParseError
    {
        ParseError {
            offset: self.length.saturating_sub(self.rest.len()),
            kind,
        }
    }

    /// Consume JSON whitespace, not arbitrary Unicode whitespace.
    ///
    /// # Specification
    /// trivial.
    fn whitespace(&mut self)
    {
        self.rest = self.rest.trim_ascii_start();
    }

    /// Split a validated byte range and advance the cursor.
    ///
    /// # Specification
    /// - ensures: returned bytes are the consumed prefix.
    /// - fails: truncated input.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns `Syntax` for a truncated token.
    ///
    /// # Adequacy
    /// - hypothesis: truncated tokens distinguish checked slicing failures.
    /// - witness: parser conformance tests.
    fn take(
        &mut self,
        width: Width,
    ) -> Result<Fragment<'source>, ParseError>
    {
        let (head, tail) = self
            .rest
            .split_at_checked(width.0)
            .ok_or_else(|| self.error(ParseErrorKind::Syntax))?;
        self.rest = tail;
        Ok(Fragment(head))
    }

    /// Open an object or array.
    ///
    /// # Specification
    /// - ensures: opening punctuation is consumed.
    /// - fails: the next token is not the requested container.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns `Syntax` for missing punctuation.
    ///
    /// # Adequacy
    /// - hypothesis: wrong container kinds cannot parse as valid schema values.
    /// - witness: parser conformance tests.
    fn open(
        &mut self,
        delimiter: Delimiter,
    ) -> Result<Sequence, ParseError>
    {
        self.whitespace();
        let punctuation = match delimiter {
            | Delimiter::Object => b'{',
            | Delimiter::Array => b'[',
        };
        self.rest = self
            .rest
            .strip_prefix(&[punctuation])
            .ok_or_else(|| self.error(ParseErrorKind::Syntax))?;
        Ok(Sequence {
            delimiter,
            first: true,
        })
    }

    /// Advance a container, rejecting omitted separators and trailing commas.
    ///
    /// # Specification
    /// - ensures: `Next` points at a value; `End` consumes closing punctuation.
    /// - fails: missing separator, trailing comma, or truncated container.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns `Syntax` for invalid punctuation.
    ///
    /// # Adequacy
    /// - hypothesis: empty, single and multiple members distinguish separator
    ///   states.
    /// - witness: parser conformance tests.
    fn next(
        &mut self,
        sequence: &mut Sequence,
    ) -> Result<Member, ParseError>
    {
        self.whitespace();
        let close = match sequence.delimiter {
            | Delimiter::Object => b'}',
            | Delimiter::Array => b']',
        };
        if let Some(rest) = self.rest.strip_prefix(&[close]) {
            self.rest = rest;
            return Ok(Member::End);
        }
        if !sequence.first {
            self.rest = self
                .rest
                .strip_prefix(b",")
                .ok_or_else(|| self.error(ParseErrorKind::Syntax))?;
            self.whitespace();
            if self.rest.starts_with(&[close]) {
                return Err(self.error(ParseErrorKind::Syntax));
            }
        }
        sequence.first = false;
        Ok(Member::Next)
    }

    /// Dispatch literal schema keys by discriminating prefix bytes.
    ///
    /// # Specification
    /// - ensures: returns a recognized literal key and consumes its colon.
    /// - fails: unknown or escaped key, incomplete token, or missing colon.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns `UnknownField` or `Syntax`.
    ///
    /// # Adequacy
    /// - hypothesis: common prefixes must not conflate distinct schema fields.
    /// - witness: schema field and ordering tests.
    #[inline]
    fn key(&mut self) -> Result<Fragment<'source>, ParseError>
    {
        self.rest = self
            .rest
            .strip_prefix(b"\"")
            .ok_or_else(|| self.error(ParseErrorKind::Syntax))?;
        let key: &[u8] = match *self.rest {
            | [b'v', ..] => b"version",
            | [b'r', b'e', b'v', ..] => b"revision",
            | [b'r', b'e', b'q', ..] => b"requires",
            | [b'r', b'u', ..] => b"rules",
            | [b'w', ..] => b"work-directory",
            | [b'p', b'r', b'i', ..] => b"primary-output",
            | [b'p', b'r', b'o', ..] => b"provides",
            | [b'o', ..] => b"outputs",
            | [b'l', b'o', b'g', ..] => b"logical-name",
            | [b'l', b'o', b'o', ..] => b"lookup-method",
            | [b's', ..] => b"source-path",
            | [b'c', ..] => b"compiled-module-path",
            | [b'u', ..] => b"unique-on-source-path",
            | [b'i', ..] => b"is-interface",
            | _ => return Err(self.error(ParseErrorKind::UnknownField)),
        };
        self.rest = self
            .rest
            .strip_prefix(key)
            .and_then(|tail| tail.strip_prefix(b"\""))
            .ok_or_else(|| self.error(ParseErrorKind::UnknownField))?;
        self.whitespace();
        self.rest = self
            .rest
            .strip_prefix(b":")
            .ok_or_else(|| self.error(ParseErrorKind::Syntax))?;
        self.whitespace();
        Ok(Fragment(key))
    }

    /// Reject a second occurrence of an object field.
    ///
    /// # Specification
    /// - ensures: marks the field on success.
    /// - fails: the same field has already appeared.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns `DuplicateField`.
    ///
    /// # Adequacy
    /// - hypothesis: duplicate fields are rejected even when their values
    ///   agree.
    /// - witness: parser conformance tests.
    fn mark(
        &self,
        seen: &mut Field,
        field: Field,
    ) -> Result<(), ParseError>
    {
        if seen.0 & field.0 != 0 {
            return Err(self.error(ParseErrorKind::DuplicateField));
        }
        seen.0 |= field.0;
        Ok(())
    }

    /// Parse an unsigned JSON integer with checked accumulation.
    ///
    /// # Specification
    /// - ensures: no overflow and no leading zero on multi-digit integers.
    /// - fails: absent digits, leading zero, or value above `u32::MAX`.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns `Integer`; surrounding punctuation rejects fractional suffixes.
    ///
    /// # Adequacy
    /// - hypothesis: zero, maximum and overflow boundaries distinguish integer
    ///   faults.
    /// - witness: parser conformance tests.
    fn number(&mut self) -> Result<Number, ParseError>
    {
        self.whitespace();
        let mut value = 0_u32;
        let mut length = 0_usize;
        for byte in self.rest.iter().copied().take_while(u8::is_ascii_digit) {
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u32::from(byte.saturating_sub(b'0'))))
                .ok_or_else(|| self.error(ParseErrorKind::Integer))?;
            length = length.saturating_add(1);
        }
        if length == 0 || (length > 1 && self.rest.starts_with(b"0")) {
            return Err(self.error(ParseErrorKind::Integer));
        }
        self.take(Width(length))?;
        Ok(Number(value))
    }

    /// Parse an exact Boolean keyword.
    ///
    /// # Specification
    /// - ensures: consumes `true` or `false`.
    /// - fails: neither keyword is present.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns `Syntax`.
    ///
    /// # Adequacy
    /// - hypothesis: both Boolean values survive through the wire model.
    /// - witness: parser conformance tests.
    fn boolean(&mut self) -> Result<Boolean, ParseError>
    {
        self.whitespace();
        if let Some(rest) = self.rest.strip_prefix(b"true") {
            self.rest = rest;
            Ok(Boolean(true))
        }
        else if let Some(rest) = self.rest.strip_prefix(b"false") {
            self.rest = rest;
            Ok(Boolean(false))
        }
        else {
            Err(self.error(ParseErrorKind::Syntax))
        }
    }

    /// Decode four hexadecimal ASCII bytes through the pair lookup table.
    ///
    /// # Specification
    /// - ensures: accepts either ASCII letter case.
    /// - fails: truncated input or a non-hexadecimal byte.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns `String` for invalid code units.
    ///
    /// # Adequacy
    /// - hypothesis: all ASCII pairs distinguish table indexing and validity
    ///   errors.
    /// - witness: `hex_pairs`.
    fn hex(&mut self) -> Result<CodeUnit, ParseError>
    {
        let bytes = self
            .rest
            .get(.. 4)
            .ok_or_else(|| self.error(ParseErrorKind::String))?;
        let &[a, b, c, d] = bytes
        else {
            return Err(self.error(ParseErrorKind::String));
        };
        let first = HEX_PAIRS
            .get(usize::from(u16::from_le_bytes([a, b])))
            .copied()
            .ok_or_else(|| self.error(ParseErrorKind::String))?;
        let second = HEX_PAIRS
            .get(usize::from(u16::from_le_bytes([c, d])))
            .copied()
            .ok_or_else(|| self.error(ParseErrorKind::String))?;
        if first == u32::MAX || second == u32::MAX {
            return Err(self.error(ParseErrorKind::String));
        }
        self.take(Width(4))?;
        Ok(CodeUnit((first << 8) | second))
    }

    /// Unescape scanner string values, borrowing the ordinary path.
    ///
    /// # Specification
    /// - requires: scanner-produced string content; encoding is not validated.
    /// - ensures: unescaped values borrow input bytes; escapes allocate lazily.
    /// - fails: invalid escapes, lone surrogates, or missing closing quote.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns `String` or a cursor failure.
    ///
    /// # Adequacy
    /// - hypothesis: escape boundaries distinguish lost prefixes and suffixes.
    /// - witness: retained escape and Unicode boundary tests.
    fn string(&mut self) -> Result<Text<'source>, ParseError>
    {
        self.whitespace();
        self.rest = self
            .rest
            .strip_prefix(b"\"")
            .ok_or_else(|| self.error(ParseErrorKind::String))?;
        let mut text = Cow::Borrowed(&[][..]);
        loop {
            let end = self
                .finder
                .find(self.rest)
                .ok_or_else(|| self.error(ParseErrorKind::String))?;
            let prefix = self.take(Width(end))?;
            if let Some(rest) = self.rest.strip_prefix(b"\"") {
                self.rest = rest;
                match text {
                    | Cow::Borrowed(_) => return Ok(Text(Cow::Borrowed(prefix.0))),
                    | Cow::Owned(ref mut bytes) => bytes.extend_from_slice(prefix.0),
                }
                return Ok(Text(text));
            }
            self.rest = self
                .rest
                .strip_prefix(b"\\")
                .ok_or_else(|| self.error(ParseErrorKind::String))?;
            let escape = self.take(Width(1))?;
            let bytes: &[u8] = match escape.0 {
                | b"\"" => b"\"",
                | b"\\" => b"\\",
                | b"/" => b"/",
                | b"b" => b"\x08",
                | b"f" => b"\x0c",
                | b"n" => b"\n",
                | b"r" => b"\r",
                | b"t" => b"\t",
                | b"u" => {
                    let unit = self.hex()?;
                    let scalar = if (0xd800 ..= 0xdbff).contains(&unit.0) {
                        self.rest = self
                            .rest
                            .strip_prefix(br"\u")
                            .ok_or_else(|| self.error(ParseErrorKind::String))?;
                        let low = self.hex()?;
                        if !(0xdc00 ..= 0xdfff).contains(&low.0) {
                            return Err(self.error(ParseErrorKind::String));
                        }
                        0x1_0000_u32.saturating_add(
                            (unit.0.saturating_sub(0xd800) << 10) | low.0.saturating_sub(0xdc00),
                        )
                    }
                    else {
                        unit.0
                    };
                    let scalar =
                        char::from_u32(scalar).ok_or_else(|| self.error(ParseErrorKind::String))?;
                    scalar.encode_utf8(&mut self.utf8).as_bytes()
                },
                | _ => return Err(self.error(ParseErrorKind::String)),
            };
            let additional = prefix.0.len().saturating_add(bytes.len());
            match text {
                | Cow::Borrowed(_) => text = Cow::Owned(Vec::with_capacity(additional)),
                | Cow::Owned(ref mut owned) => owned.reserve(additional),
            }
            let owned = text.to_mut();
            owned.extend_from_slice(prefix.0);
            owned.extend_from_slice(bytes);
        }
    }

    /// Parse an array using the schema's nonrecursive element parser.
    ///
    /// # Specification
    /// - ensures: preserves element order and validates all separators.
    /// - fails: invalid element or container syntax.
    /// - panics: none.
    ///
    /// # Errors
    /// Propagates element or punctuation failures.
    ///
    /// # Adequacy
    /// - hypothesis: multiple rules and modules distinguish truncation and
    ///   reordering.
    /// - witness: parser conformance tests.
    fn array<T>(
        &mut self,
        mut element: impl FnMut(&mut Self) -> Result<T, ParseError>,
    ) -> Result<Vec<T>, ParseError>
    {
        let mut sequence = self.open(Delimiter::Array)?;
        let mut values = Vec::new();
        while matches!(self.next(&mut sequence)?, Member::Next) {
            let value = element(self)?;
            values.push(value);
        }
        Ok(values)
    }

    /// Parse a provided or required module without discarding identity
    /// metadata.
    ///
    /// # Specification
    /// - ensures: retains identity, locations, interface status and lookup
    ///   mode.
    /// - fails: missing name, duplicate/unknown fields or invalid values.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns the schema or JSON failure.
    ///
    /// # Adequacy
    /// - hypothesis: path-unique identities and nondefault lookup modes remain
    ///   distinguishable.
    /// - witness: parser conformance tests.
    fn module(
        &mut self,
        role: ModuleRole,
    ) -> Result<ParsedModule<'source>, ParseError>
    {
        let mut sequence = self.open(Delimiter::Object)?;
        let mut seen = Field(0);
        let mut name = None;
        let mut source_path = None;
        let mut compiled_module_path = None;
        let mut unique_on_source_path = None;
        let mut interface = IsInterface::default();
        let mut lookup = LookupMethod::default();
        while matches!(self.next(&mut sequence)?, Member::Next) {
            let key = self.key()?;
            let field = match key.0 {
                | b"logical-name" => Field(1),
                | b"source-path" => Field(2),
                | b"compiled-module-path" => Field(4),
                | b"unique-on-source-path" => Field(8),
                | b"is-interface" if matches!(role, ModuleRole::Provided) => Field(16),
                | b"lookup-method" if matches!(role, ModuleRole::Required) => Field(32),
                | _ => return Err(self.error(ParseErrorKind::UnknownField)),
            };
            self.mark(&mut seen, field)?;
            match field.0 {
                | 1 => {
                    let text = self.string()?;
                    name = Some(ModuleName(text.0));
                },
                | 2 => {
                    let text = self.string()?;
                    source_path = Some(text.0);
                },
                | 4 => {
                    let text = self.string()?;
                    compiled_module_path = Some(text.0);
                },
                | 8 => {
                    let value = self.boolean()?;
                    unique_on_source_path = Some(value.0);
                },
                | 16 => {
                    let value = self.boolean()?;
                    interface = IsInterface(value.0);
                },
                | 32 => {
                    let text = self.string()?;
                    lookup = match text.0.as_ref() {
                        | b"by-name" => LookupMethod::ByName,
                        | b"include-angle" => LookupMethod::IncludeAngle,
                        | b"include-quote" => LookupMethod::IncludeQuote,
                        | _ => return Err(self.error(ParseErrorKind::LookupMethod)),
                    };
                },
                | _ => return Err(self.error(ParseErrorKind::UnknownField)),
            }
        }
        let logical_name = name.ok_or_else(|| self.error(ParseErrorKind::MissingField))?;
        Ok(ParsedModule {
            desc: ModuleDesc {
                logical_name,
                source_path,
                compiled_module_path,
                unique_on_source_path,
            },
            interface,
            lookup,
        })
    }

    /// Parse a complete translation-unit rule.
    ///
    /// # Specification
    /// - ensures: preserves all rule metadata and module descriptions.
    /// - fails: duplicate/unknown fields or invalid values.
    /// - panics: none.
    ///
    /// # Errors
    /// Propagates schema and token failures.
    ///
    /// # Adequacy
    /// - hypothesis: nonempty output and module metadata distinguish lossy
    ///   parsing.
    /// - witness: parser conformance tests.
    fn rule(&mut self) -> Result<DepInfo<'source>, ParseError>
    {
        let mut sequence = self.open(Delimiter::Object)?;
        let mut seen = Field(0);
        let mut rule = DepInfo::default();
        while matches!(self.next(&mut sequence)?, Member::Next) {
            let key = self.key()?;
            let field = match key.0 {
                | b"work-directory" => Field(1),
                | b"primary-output" => Field(2),
                | b"outputs" => Field(4),
                | b"provides" => Field(8),
                | b"requires" => Field(16),
                | _ => return Err(self.error(ParseErrorKind::UnknownField)),
            };
            self.mark(&mut seen, field)?;
            match field.0 {
                | 1 => {
                    let text = self.string()?;
                    rule.work_directory = Some(text.0);
                },
                | 2 => {
                    let text = self.string()?;
                    rule.primary_output = Some(text.0);
                },
                | 4 => {
                    rule.outputs = self.array(|parser| {
                        let text = parser.string()?;
                        Ok(text.0)
                    })?;
                },
                | 8 => {
                    rule.provides = self.array(|parser| {
                        let module = parser.module(ModuleRole::Provided)?;
                        Ok(ProvidedModuleDesc {
                            desc: module.desc,
                            is_interface: module.interface,
                        })
                    })?;
                },
                | 16 => {
                    rule.requires = self.array(|parser| {
                        let module = parser.module(ModuleRole::Required)?;
                        Ok(RequiredModuleDesc {
                            desc: module.desc,
                            lookup_method: module.lookup,
                        })
                    })?;
                },
                | _ => return Err(self.error(ParseErrorKind::UnknownField)),
            }
        }
        Ok(rule)
    }
}

/// Which module fields the schema admits.
#[derive(Clone, Copy)]
enum ModuleRole
{
    /// Provider with interface status.
    Provided,
    /// Requirement with lookup mode.
    Required,
}
/// Temporary module record shared by the two wire shapes.
struct ParsedModule<'source>
{
    /// Identity and paths.
    desc: ModuleDesc<'source>,
    /// Interface status for providers.
    interface: IsInterface,
    /// Lookup method for requirements.
    lookup: LookupMethod,
}

/// Parse a complete dependency document.
///
/// # Specification
/// - ensures: consumes all input and preserves the revision 5 fields.
/// - fails: malformed tokens, missing or duplicate fields.
/// - panics: none.
///
/// # Errors
/// Returns the named JSON or schema failure with its offset.
///
/// # Adequacy
/// - hypothesis: complete documents and near-miss mutations distinguish
///   boundary failures.
/// - witness: parser conformance tests.
pub fn parse(input: JsonInput<'_>) -> Result<DepFile<'_>, ParseError>
{
    #[cfg(all(
        target_arch = "x86_64",
        any(target_feature = "avx2", target_feature = "sse2")
    ))]
    let finder = StringFinder::new(b'"', b'\\').ok_or(ParseError {
        offset: 0,
        kind: ParseErrorKind::Syntax,
    })?;
    #[cfg(not(all(
        target_arch = "x86_64",
        any(target_feature = "avx2", target_feature = "sse2")
    )))]
    let finder = StringFinder::new(b'"', b'\\');
    let mut parser = Parser {
        rest: input.0,
        length: input.0.len(),
        finder,
        utf8: [0; 4],
    };
    let mut sequence = parser.open(Delimiter::Object)?;
    let mut seen = Field(0);
    let mut version = None;
    let mut revision = None;
    let mut rules = None;
    while matches!(parser.next(&mut sequence)?, Member::Next) {
        let key = parser.key()?;
        let field = match key.0 {
            | b"version" => Field(1),
            | b"revision" => Field(2),
            | b"rules" => Field(4),
            | _ => return Err(parser.error(ParseErrorKind::UnknownField)),
        };
        parser.mark(&mut seen, field)?;
        match field.0 {
            | 1 => {
                let number = parser.number()?;
                version = Some(Version(number.0));
            },
            | 2 => {
                let number = parser.number()?;
                revision = Some(Revision(number.0));
            },
            | 4 => {
                rules = {
                    let rules = parser.array(Parser::rule)?;
                    Some(rules)
                };
            },
            | _ => return Err(parser.error(ParseErrorKind::UnknownField)),
        }
    }
    parser.whitespace();
    if !parser.rest.is_empty() {
        return Err(parser.error(ParseErrorKind::Syntax));
    }
    let version = version.ok_or_else(|| parser.error(ParseErrorKind::MissingField))?;
    let rules = rules.ok_or_else(|| parser.error(ParseErrorKind::MissingField))?;
    Ok(DepFile {
        version,
        revision,
        rules,
    })
}
