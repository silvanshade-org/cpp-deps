//! Wire-format boundaries and borrowed-string evidence.

#[test]
fn hex_pairs()
{
    for index in u16::MIN ..= u16::MAX {
        let bytes = index.to_le_bytes();
        let expected = if bytes.iter().all(u8::is_ascii_hexdigit) {
            let text = core::str::from_utf8(&bytes).expect("ASCII hexadecimal");
            u32::from_str_radix(text, 16).expect("valid byte")
        }
        else {
            u32::MAX
        };
        assert_eq!(super::HEX_PAIRS.get(usize::from(index)), Some(&expected));
    }
}

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;

use crate::JsonInput;
use crate::ParseErrorKind;
use crate::r5::DepFile;
use crate::r5::IsInterface;
use crate::r5::LookupMethod;
use crate::r5::Version;

#[test]
fn metadata_and_borrowing()
{
    let source = br#"{"rules":[{"outputs":["a.pcm","a.d"],"work-directory":"/build","primary-output":"a.o","requires":[{"compiled-module-path":"b.pcm","source-path":"b.hpp","unique-on-source-path":true,"logical-name":"b","lookup-method":"include-angle"}],"provides":[{"logical-name":"a","is-interface":false}]}],"revision":5,"version":1}"#;
    let file = DepFile::parse(JsonInput(source)).expect("valid dependency file");
    assert_eq!(file.version, Version(1));
    let rule = file.rules.first().expect("one rule");
    assert_eq!(rule.outputs, [b"a.pcm".as_slice(), b"a.d"]);
    assert_eq!(rule.work_directory.as_deref(), Some(b"/build".as_slice()));
    assert_eq!(rule.primary_output.as_deref(), Some(b"a.o".as_slice()));
    let required = rule.requires.first().expect("requirement");
    assert_eq!(required.lookup_method, LookupMethod::IncludeAngle);
    assert_eq!(
        required.desc.source_path.as_deref(),
        Some(b"b.hpp".as_slice())
    );
    assert_eq!(
        required.desc.compiled_module_path.as_deref(),
        Some(b"b.pcm".as_slice())
    );
    assert_eq!(required.desc.unique_on_source_path, Some(true));
    assert!(matches!(required.desc.logical_name.0, Cow::Borrowed(b"b")));
    let provided = rule.provides.first().expect("provider");
    assert_eq!(provided.is_interface, IsInterface(false));
    assert!(matches!(provided.desc.logical_name.0, Cow::Borrowed(b"a")));
}

#[test]
fn json_strings_agree_with_independent_decoder()
{
    let cases = [
        r#""plain""#,
        r#""""#,
        r#""路径/é""#,
        r#""abcdefghijklmnopqrstuvwxyz路径""#,
        r#""\"\\\/\b\f\n\r\t""#,
        r#""\u0000\u007f\u0080\u07ff\u0800\uFFFF""#,
        r#""\ud800\udc00\uDBFF\uDFFF""#,
        r#""prefix\u0061suffix""#,
    ];
    for encoded in cases {
        let expected: String = serde_json::from_str(encoded).expect("independent JSON string");
        let document =
            format!(r#"{{"version":1,"rules":[{{"provides":[{{"logical-name":{encoded}}}]}}]}}"#);
        let file = DepFile::parse(JsonInput(document.as_bytes())).expect("valid string");
        let provided = file
            .rules
            .first()
            .expect("rule")
            .provides
            .first()
            .expect("provider");
        assert_eq!(provided.desc.logical_name.as_ref(), expected.as_bytes());
        assert_eq!(provided.is_interface, IsInterface(true));
        assert_eq!(
            matches!(provided.desc.logical_name.0, Cow::Owned(_)),
            encoded.contains('\\')
        );
    }
}

#[test]
fn rejects_malformed_tokens_and_schema()
{
    let cases: &[(&[u8], ParseErrorKind)] = &[
        (br#"{"version":1,"rules":[],}"#, ParseErrorKind::Syntax),
        (br#"{"version":1,"rules":[{},]}"#, ParseErrorKind::Syntax),
        (br#"{"version":1,"rules":[]} false"#, ParseErrorKind::Syntax),
        (br#"{"version":01,"rules":[]}"#, ParseErrorKind::Integer),
        (
            br#"{"version":4294967296,"rules":[]}"#,
            ParseErrorKind::Integer,
        ),
        (br#"{"version":-1,"rules":[]}"#, ParseErrorKind::Integer),
        (
            br#"{"version":1,"version":1,"rules":[]}"#,
            ParseErrorKind::DuplicateField,
        ),
        (
            br#"{"version":1,"rules":[{"provides":[],"provides":[]}]}"#,
            ParseErrorKind::DuplicateField,
        ),
        (br#"{"version":1}"#, ParseErrorKind::MissingField),
        (
            br#"{"version":1,"rules":[{"provides":[{}]}]}"#,
            ParseErrorKind::MissingField,
        ),
        (
            br#"{"version":1,"rules":[],"unknown":0}"#,
            ParseErrorKind::UnknownField,
        ),
        (
            br#"{"version":1,"rules":[{"requires":[{"logical-name":"a","lookup-method":"bad"}]}]}"#,
            ParseErrorKind::LookupMethod,
        ),
        (
            br#"{"version":1,"rules":[{"outputs":["\uD800"]}]}"#,
            ParseErrorKind::String,
        ),
        (
            br#"{"version":1,"rules":[{"outputs":["\uDC00"]}]}"#,
            ParseErrorKind::String,
        ),
        (
            br#"{"version":1,"rules":[{"outputs":["\uD800\u0041"]}]}"#,
            ParseErrorKind::String,
        ),
        (
            br#"{"version":1,"rules":[{"outputs":["\u00GG"]}]}"#,
            ParseErrorKind::String,
        ),
        (
            br#"{"version":1,"rules":[{"outputs":["\x"]}]}"#,
            ParseErrorKind::String,
        ),
    ];
    for &(document, expected) in cases {
        assert_eq!(
            DepFile::parse(JsonInput(document))
                .expect_err("invalid document")
                .kind,
            expected
        );
    }
}

#[test]
fn field_order_and_integer_boundary()
{
    let input = br#"{"rules":[],"version":4294967295}"#;
    assert_eq!(
        DepFile::parse(JsonInput(input))
            .expect("maximum version")
            .version,
        Version(u32::MAX)
    );
    let input = br#"{"version":0,"rules":[]}"#;
    assert_eq!(
        DepFile::parse(JsonInput(input))
            .expect("zero version")
            .version,
        Version(0)
    );
}

#[test]
fn schema_field_regressions()
{
    let cases = [
        ("duplicate_field_revision", br#"{"version":1,"revision":0,"rules":[],"revision":0,}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_rules", br#"{"version":1,"revision":0,"rules":[],"rules":[],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_version", br#"{"version":1,"revision":0,"rules":[],"version":1,}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("missing_field_rules", br#"{"version":1}"#.as_slice(), ParseErrorKind::MissingField),
        ("missing_field_version", br#"{"rules":[]}"#.as_slice(), ParseErrorKind::MissingField),
        ("mismatch_field_revision", br#"{"version":1,"r#vision":0,"rules":[],}"#.as_slice(), ParseErrorKind::UnknownField),
        ("mismatch_field_rules", br#"{"version":1,"revision":0,"r#les":[],}"#.as_slice(), ParseErrorKind::UnknownField),
        ("mismatch_field", br#"{"bad":[],"version":1,"revision":0,"rules":[],}"#.as_slice(), ParseErrorKind::UnknownField),
        ("mismatch_field_unquoted", br#"{"version":1,"revision":0,bad:[],"rules":[],}"#.as_slice(), ParseErrorKind::Syntax),
        ("duplicate_field_outputs", br#"{"version":1,"revision":0,"rules":[{"outputs":[],"outputs":[],}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_primary_output", br#"{"version":1,"revision":0,"rules":[{"primary-output":"foo.cpp","primary-output":"foo.cpp"}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_provides", br#"{"version":1,"revision":0,"rules":[{"provides":[],"provides":[]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_requires", br#"{"version":1,"revision":0,"rules":[{"requires":[],"requires":[]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_work_directory", br#"{"version":1,"revision":0,"rules":[{"work-directory":"build","work-directory":"build"}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("mismatch_field_primary_output", br#"{"version":1,"revision":0,"rules":[{"prxmary-output":"foo.cpp","provides":[]}],}"#.as_slice(), ParseErrorKind::UnknownField),
        ("mismatch_field_provides", br#"{"version":1,"revision":0,"rules":[{"primary-output":"foo.cpp","prxvides":[]}],}"#.as_slice(), ParseErrorKind::UnknownField),
        ("mismatch_field_pr", br#"{"version":1,"revision":0,"rules":[{"px":[]}],}"#.as_slice(), ParseErrorKind::UnknownField),
        ("mismatch_field", br#"{"version":1,"revision":0,"rules":[{"x":[]}],}"#.as_slice(), ParseErrorKind::UnknownField),
        ("mismatch_field_unquoted", br#"{"version":1,"revision":0,"rules":[{primary-output:"foo.cpp"}],}"#.as_slice(), ParseErrorKind::Syntax),
        ("duplicate_field_source_path", br#"{"version":1,"revision":0,"rules":[{"provides":[{"source-path":"foo.cpp","source-path":"foo.cpp"}]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_compiled_module_path", br#"{"version":1,"revision":0,"rules":[{"provides":[{"compiled-module-path":"foo.o","compiled-module-path":"foo.o"}]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_logical_name", br#"{"version":1,"revision":0,"rules":[{"provides":[{"logical-name":"foo","logical-name":"foo"}]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_unique_on_source_path", br#"{"version":1,"revision":0,"rules":[{"provides":[{"unique-on-source-path":false,"unique-on-source-path":false}]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_is_interface", br#"{"version":1,"revision":0,"rules":[{"provides":[{"is-interface":false,"is-interface":false}]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("mismatch_field", br#"{"version":1,"revision":0,"rules":[{"provides":[{"x":[]}]}],}"#.as_slice(), ParseErrorKind::UnknownField),
        ("mismatch_field_unquoted", br#"{"version":1,"revision":0,"rules":[{"provides":[{x:[]}]}],}"#.as_slice(), ParseErrorKind::Syntax),
        ("duplicate_field_source_path", br#"{"version":1,"revision":0,"rules":[{"requires":[{"source-path":"foo.cpp","source-path":"foo.cpp"}]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_compiled_module_path", br#"{"version":1,"revision":0,"rules":[{"requires":[{"compiled-module-path":"foo.o","compiled-module-path":"foo.o"}]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_logical_name", br#"{"version":1,"revision":0,"rules":[{"requires":[{"logical-name":"foo","logical-name":"foo"}]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_unique_on_source_path", br#"{"version":1,"revision":0,"rules":[{"requires":[{"unique-on-source-path":false,"unique-on-source-path":false}]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("duplicate_field_lookup_method", br#"{"version":1,"revision":0,"rules":[{"requires":[{"lookup-method":"by-name","lookup-method":"by-name"}]}],}"#.as_slice(), ParseErrorKind::DuplicateField),
        ("mismatch_field_l", br#"{"version":1,"revision":0,"rules":[{"requires":[{"lx":[]}]}],}"#.as_slice(), ParseErrorKind::UnknownField),
        ("mismatch_field", br#"{"version":1,"revision":0,"rules":[{"requires":[{"x":[]}]}],}"#.as_slice(), ParseErrorKind::UnknownField),
        ("mismatch_field_unquoted", br#"{"version":1,"revision":0,"rules":[{"requires":[{x:[]}]}],}"#.as_slice(), ParseErrorKind::Syntax),
        ("mismatch_field_include", br#"{"version":1,"revision":0,"rules":[{"requires":[{"logical-name":"foo","lookup-method":"invalid"}]}],}"#.as_slice(), ParseErrorKind::LookupMethod),
        ("mismatch_field_by_name_include", br#"{"version":1,"revision":0,"rules":[{"requires":[{"logical-name":"foo","lookup-method":"wrong"}]}],}"#.as_slice(), ParseErrorKind::LookupMethod),
        ("mismatch_field_unquoted", br#"{"version":1,"revision":0,"rules":[{"requires":[{"logical-name":"foo","lookup-method":bad}]}],}"#.as_slice(), ParseErrorKind::String),
    ];
    for (name, input, expected) in cases {
        assert_eq!(
            DepFile::parse(JsonInput(input)).expect_err(name).kind,
            expected,
            "{name}: {input:?}"
        );
    }
}

#[test]
fn escaped_output_paths()
{
    for (escaped, expected) in [(r"fo\u2764o.o", "fo❤o.o"), (r"fo\uD834\uDD1Eo.o", "fo𝄞o.o")] {
        let input = format!(r#"{{"version":1,"rules":[{{"primary-output":"{escaped}"}}]}}"#);
        let file = DepFile::parse(JsonInput(input.as_bytes())).expect("escaped output");
        let output = file
            .rules
            .first()
            .expect("rule")
            .primary_output
            .as_ref()
            .expect("output");
        assert_eq!(output.as_ref(), expected.as_bytes());
        assert!(matches!(output, Cow::Owned(_)));
    }
}

#[test]
fn scanner_bytes_are_preserved_without_utf8_validation()
{
    let input = b"{\"version\":1,\"rules\":[{\"outputs\":[\"a\xff\xc0\",\"b\xff\\n\"]}]}";
    let file = DepFile::parse(JsonInput(input)).expect("opaque scanner strings");
    let outputs = &file.rules.first().expect("rule").outputs;
    assert_eq!(outputs, &[b"a\xff\xc0".as_slice(), b"b\xff\n"]);
    assert!(matches!(outputs.first(), Some(Cow::Borrowed(_))));
    assert!(matches!(outputs.get(1), Some(Cow::Owned(_))));
}

#[test]
fn unicode_scalars_round_trip_through_utf16_escapes()
{
    use core::fmt::Write as _;
    for scalar in 0 ..= 0x10_ffff {
        let Some(character) = char::from_u32(scalar)
        else {
            continue;
        };
        let mut utf16 = [0_u16; 2];
        let mut input = String::from(r#"{"version":1,"rules":[{"outputs":["prefix"#);
        for unit in character.encode_utf16(&mut utf16) {
            write!(input, "\\u{unit:04x}").expect("String writer");
        }
        input.push_str(r#"suffix"]}]}"#);
        let file = DepFile::parse(JsonInput(input.as_bytes())).expect("UTF-16 escapes");
        let output = file
            .rules
            .first()
            .expect("rule")
            .outputs
            .first()
            .expect("output");
        let expected = format!("prefix{character}suffix");
        assert_eq!(output.as_ref(), expected.as_bytes(), "U+{scalar:04X}");
    }
}
