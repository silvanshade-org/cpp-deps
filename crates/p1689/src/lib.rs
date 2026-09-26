#![no_std]
//! Owned data model for P1689 revision 5 C++ module dependency files.

extern crate alloc;

/// P1689 revision 5 dependency descriptions.
pub mod r5
{
    use alloc::string::String;
    use alloc::vec::Vec;

    use serde::Deserialize;
    use serde::Serialize;

    /// Wire-format version of a dependency file.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[repr(transparent)]
    #[serde(transparent)]
    pub struct Version(pub u32);

    /// Wire-format revision of a dependency file.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[repr(transparent)]
    #[serde(transparent)]
    pub struct Revision(pub u32);

    /// Logical name used to match a provided module with an import.
    #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
    #[repr(transparent)]
    #[serde(transparent)]
    pub struct ModuleName(pub String);

    impl AsRef<str> for ModuleName
    {
        /// Borrow the module's logical name.
        ///
        /// # Specification
        /// trivial.
        #[inline]
        fn as_ref(&self) -> &str
        {
            &self.0
        }
    }

    /// Whether a provided unit exports a compiled module interface.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[repr(transparent)]
    #[serde(transparent)]
    pub struct IsInterface(pub bool);

    impl Default for IsInterface
    {
        /// Use the P1689 default for an omitted `is-interface` field.
        ///
        /// # Specification
        /// - ensures: an omitted field denotes an interface, not an
        ///   implementation.
        /// - panics: none.
        ///
        /// # Adequacy
        /// - hypothesis: L3 checks the omitted field and explicit false on
        ///   valid dependency files.
        /// - witness: `r5::tests::omitted_interface_defaults_to_true`
        #[inline]
        fn default() -> Self
        {
            Self(true)
        }
    }

    /// A P1689 revision 5 dependency file containing one or more rules.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct DepFile
    {
        /// Format version, usually 1.
        pub version: Version,
        /// Optional format revision, usually 5.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub revision: Option<Revision>,
        /// Dependency rules produced by the scanner.
        pub rules: Vec<DepInfo>,
    }

    /// Dependency information associated with one translation unit.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct DepInfo
    {
        /// Directory used for relative paths in the rule.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub work_directory: Option<String>,
        /// Object or other primary compiler output.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub primary_output: Option<String>,
        /// Additional outputs from the same compilation.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub outputs: Vec<String>,
        /// Modules made available by this unit.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub provides: Vec<ProvidedModuleDesc>,
        /// Modules needed before this unit can compile.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub requires: Vec<RequiredModuleDesc>,
    }

    /// Logical name and optional source or compiled-module locations.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct ModuleDesc
    {
        /// Logical module identity, including a partition suffix when present.
        pub logical_name: ModuleName,
        /// Path identifying a source-unique module, if supplied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub source_path: Option<String>,
        /// Compiler-supplied interface path, if supplied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub compiled_module_path: Option<String>,
        /// Whether `source-path` rather than `logical-name` is unique.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub unique_on_source_path: Option<bool>,
    }

    /// Module provided by a translation unit.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct ProvidedModuleDesc
    {
        /// Identity and optional paths of the provided module.
        #[serde(flatten)]
        pub desc: ModuleDesc,
        /// Defaults to an interface under P1689 when omitted.
        #[serde(default)]
        pub is_interface: IsInterface,
    }

    /// Way a module requirement is resolved.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub enum LookupMethod
    {
        /// Import by logical module name.
        #[default]
        ByName,
        /// Import an angle-bracket header unit.
        IncludeAngle,
        /// Import a quoted header unit.
        IncludeQuote,
    }

    /// Module required by a translation unit.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct RequiredModuleDesc
    {
        /// Identity and optional paths of the required module.
        #[serde(flatten)]
        pub desc: ModuleDesc,
        /// Import lookup mode; defaults to module name.
        #[serde(default)]
        pub lookup_method: LookupMethod,
    }

    #[cfg(test)]
    mod tests
    {
        use super::DepFile;
        use super::IsInterface;

        #[test]
        fn p1689_fields_round_trip()
        {
            let source = r#"{"version":1,"revision":5,"rules":[{"work-directory":"build","primary-output":"unit.o","outputs":["unit.pcm"],"provides":[{"logical-name":"lib:part","source-path":"part.cppm","compiled-module-path":"unit.pcm","unique-on-source-path":false,"is-interface":true}],"requires":[{"logical-name":"base","lookup-method":"by-name"}]}]}"#;
            let decoded: DepFile =
                serde_json::from_str(source).expect("valid revision 5 dependency file");
            let encoded = serde_json::to_string(&decoded).expect("serializable dependency file");
            let decoded_again: DepFile =
                serde_json::from_str(&encoded).expect("encoded dependency file");
            assert_eq!(decoded, decoded_again);
            assert_eq!(
                decoded
                    .rules
                    .first()
                    .and_then(|rule| rule.provides.first())
                    .map(|item| item.desc.logical_name.as_ref()),
                Some("lib:part")
            );
        }

        #[test]
        fn omitted_interface_defaults_to_true()
        {
            let source = r#"{"version":1,"rules":[{"provides":[{"logical-name":"api"},{"logical-name":"implementation","is-interface":false}]}]}"#;
            let decoded: DepFile = serde_json::from_str(source).expect("valid dependency file");
            let provides = &decoded.rules.first().expect("one rule").provides;
            assert_eq!(
                provides.first().map(|item| item.is_interface),
                Some(IsInterface(true))
            );
            assert_eq!(
                provides.get(1).map(|item| item.is_interface),
                Some(IsInterface(false))
            );
        }
    }
}
