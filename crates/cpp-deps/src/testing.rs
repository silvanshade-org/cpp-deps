use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use p1689::r5::Utf8Path;
use tempdir::TempDir;

use crate::{CppDeps, CppDepsItem};

pub mod corpus;

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type BoxResult<T> = Result<T, BoxError>;

pub struct ValidateOrder<'a, P, B> {
    #[allow(unused)]
    pub out_dir: TempDir,
    #[allow(unused)]
    pub src_root: PathBuf,
    #[allow(unused)]
    pub expected_outputs: BTreeSet<&'a Utf8Path>,
    pub cpp_deps: CppDeps<P, B>,
}

impl<'a, P, B> ValidateOrder<'a, P, B>
where
    P: AsRef<Utf8Path> + Send + Sync + 'static,
    B: AsRef<[u8]> + Send + Sync + 'static,
{
    pub fn new<Is>(src_proj: impl AsRef<Path>, items: Is, expected_outputs: BTreeSet<&'a Utf8Path>) -> BoxResult<Self>
    where
        Is: IntoIterator<Item = CppDepsItem<P, B>>,
    {
        let out_dir = tempdir::TempDir::new("cpp-deps")?;
        let src_root = out_dir.path().join(src_proj.as_ref());
        self::build_script_env(out_dir.path())?;
        let mut cpp_deps = CppDeps::new()?;
        cpp_deps.items(items)?;
        #[cfg(feature = "cc")]
        {
            let mut build = cc::Build::new();
            build.std("gnu++23");
            cpp_deps.compiler(build)?;
        }
        Ok(ValidateOrder {
            out_dir,
            src_root,
            expected_outputs,
            cpp_deps,
        })
    }

    pub fn run(self) -> BoxResult<()> {
        self.cpp_deps
            .analyze()
            .validate_order(&self.src_root, self.expected_outputs)
    }
}

pub fn build_script_env(out_dir: &Path) -> BoxResult<()> {
    let out_dir = out_dir
        .as_os_str()
        .to_str()
        .ok_or_else(|| -> BoxError { "StringConversionFailure".into() })?;
    std::env::set_var("OPT_LEVEL", "3");
    std::env::set_var("OUT_DIR", out_dir);
    #[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
    {
        std::env::set_var("TARGET", "x86_64-unknown-linux-gnu");
        std::env::set_var("HOST", "x86_64-unknown-linux-gnu");
    }
    #[cfg(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc"))]
    {
        std::env::set_var("TARGET", "x86_64-pc-windows-msvc");
        std::env::set_var("HOST", "x86_64-pc-windows-msvc");
    }
    Ok(())
}
