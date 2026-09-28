#![no_std]
//! Borrowed P1689 scanner records with specialized byte parsing.

extern crate alloc;

mod parser;

pub use parser::JsonInput;
pub use parser::ParseError;
pub use parser::ParseErrorKind;

/// P1689 revision 5 dependency descriptions.
pub mod r5
{
    use alloc::borrow::Cow;
    use alloc::vec::Vec;

    /// Wire-format version of a dependency file.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    #[repr(transparent)]
    pub struct Version(pub u32);

    /// Wire-format revision of a dependency file.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    #[repr(transparent)]
    pub struct Revision(pub u32);

    /// Logical name bytes; unescaped input borrows the scanner buffer.
    #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    #[repr(transparent)]
    pub struct ModuleName<'source>(pub Cow<'source, [u8]>);

    impl AsRef<[u8]> for ModuleName<'_>
    {
        /// Borrow the logical name.
        ///
        /// # Specification
        /// trivial.
        #[inline]
        fn as_ref(&self) -> &[u8]
        {
            &self.0
        }
    }

    /// Whether a provided unit exports a compiled module interface.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    #[repr(transparent)]
    pub struct IsInterface(pub bool);

    impl Default for IsInterface
    {
        /// Apply P1689's omitted-field default.
        ///
        /// # Specification
        /// trivial.
        #[inline]
        fn default() -> Self
        {
            Self(true)
        }
    }

    /// Dependency file borrowing strings from caller-owned scanner output.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct DepFile<'source>
    {
        /// Wire-format version.
        pub version: Version,
        /// Revision when explicitly reported.
        pub revision: Option<Revision>,
        /// Translation-unit rules in scanner order.
        pub rules: Vec<DepInfo<'source>>,
    }

    impl<'source> DepFile<'source>
    {
        /// Parse a complete dependency file without copying unescaped strings.
        ///
        /// # Specification
        /// - ensures: field order does not affect the result; every recognized
        ///   field is retained, including source identity and output locations.
        /// - requires: scanner-produced JSON with literal schema keys.
        /// - fails: malformed structure, duplicate or unknown fields, invalid
        ///   escapes, missing required fields, or integer overflow.
        /// - panics: none.
        /// - intension: ordinary strings borrow bytes without UTF-8 validation;
        ///   only escaped strings allocate. Nesting follows the fixed schema.
        ///
        /// # Errors
        /// Returns a byte offset and named parsing failure.
        ///
        /// # Adequacy
        /// - hypothesis: field permutations, Unicode boundaries and malformed
        ///   tokens distinguish metadata loss and accepting invalid input.
        /// - witness: parser conformance tests.
        #[inline]
        pub fn parse(input: crate::JsonInput<'source>) -> Result<Self, crate::ParseError>
        {
            crate::parser::parse(input)
        }
    }

    /// Complete dependency rule for one translation unit.
    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct DepInfo<'source>
    {
        /// Base directory for relative rule paths.
        pub work_directory: Option<Cow<'source, [u8]>>,
        /// Primary compiler output.
        pub primary_output: Option<Cow<'source, [u8]>>,
        /// Additional compiler outputs.
        pub outputs: Vec<Cow<'source, [u8]>>,
        /// Provided modules.
        pub provides: Vec<ProvidedModuleDesc<'source>>,
        /// Imported modules.
        pub requires: Vec<RequiredModuleDesc<'source>>,
    }

    /// Module identity together with its source and compiled locations.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ModuleDesc<'source>
    {
        /// Logical name, including a partition suffix.
        pub logical_name: ModuleName<'source>,
        /// Source path identifying source-unique modules.
        pub source_path: Option<Cow<'source, [u8]>>,
        /// Compiler-reported compiled module location.
        pub compiled_module_path: Option<Cow<'source, [u8]>>,
        /// Explicit identity discriminator; omitted means logical-name
        /// identity.
        pub unique_on_source_path: Option<bool>,
    }

    /// Module provided by a translation unit.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ProvidedModuleDesc<'source>
    {
        /// Identity and locations.
        pub desc: ModuleDesc<'source>,
        /// Interface status; omitted on the wire means true.
        pub is_interface: IsInterface,
    }

    /// Resolution mode for a module requirement.
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum LookupMethod
    {
        /// Resolve a named module.
        #[default]
        ByName,
        /// Resolve an angle-bracket header unit.
        IncludeAngle,
        /// Resolve a quoted header unit.
        IncludeQuote,
    }

    /// Module required before compilation can proceed.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct RequiredModuleDesc<'source>
    {
        /// Identity and locations.
        pub desc: ModuleDesc<'source>,
        /// Required resolution mode.
        pub lookup_method: LookupMethod,
    }
}
