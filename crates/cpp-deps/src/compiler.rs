use alloc::sync::Arc;
use std::{collections::BTreeSet, process::Command};

use p1689::r5::{self, yoke::DepInfoNameYoke};

#[cfg(feature = "memchr")]
use crate::Finders;
use crate::{InnerError, InnerErrorKind};

pub(crate) struct Compiler {
    tool: crate::vendor::cc::Tool,
    family: CompilerFamily,
    out_dir: Arc<r5::Utf8PathBuf>,
    #[cfg(feature = "memchr")]
    finders: Finders,
}
impl Compiler {
    pub(crate) fn new(mut build: crate::vendor::cc::Build) -> Result<Self, InnerError> {
        build.cpp(true);
        build.std("c++20");
        let tool = build
            .try_get_compiler()
            .map_err(|err| InnerError::new(InnerErrorKind::CcTryGetCompiler { err }))?;
        let family = CompilerFamily::try_from(&tool)?;
        let out_dir = std::env::var("OUT_DIR").map_err(|err| InnerError::new(InnerErrorKind::EnvVar { err }))?;
        let out_dir = r5::Utf8PathBuf::from(out_dir);
        let out_dir = Arc::from(out_dir);
        #[cfg(feature = "memchr")]
        let finders = Finders::new()?;
        Ok(Self {
            tool,
            family,
            out_dir,
            #[cfg(feature = "memchr")]
            finders,
        })
    }

    pub(crate) fn bmi_path(
        &self,
        mut path: r5::Utf8PathBuf,
        provides: &[r5::ProvidedModuleDesc<'_>],
    ) -> Option<r5::Utf8PathBuf> {
        if self.tool.is_like_clang() || self.tool.is_like_msvc() {
            for provided in provides {
                let name = provided.desc.logical_name();
                if self.module_name_is_dotted(&name) {
                    let ext = self.family.bmi_file_ext();
                    path.set_extension(ext);
                    return Some(path);
                }
            }
        }
        None
    }

    fn dep_file_dst(&self, base: &r5::Utf8Path, src: &r5::Utf8Path) -> Result<r5::Utf8PathBuf, InnerError> {
        let ext = self.family.dep_file_ext();
        let src = src
            .strip_prefix(base)
            .map_err(|err| InnerError::new(InnerErrorKind::PathStripPrefix { err }))?;
        let dst = self.out_dir.join(src).with_extension(ext);
        Ok(dst)
    }

    fn dep_file_cmd(&self, src: &r5::Utf8Path, dst: &r5::Utf8Path) -> Result<Command, InnerError> {
        let cxx = self.tool.to_command();
        self.family.dep_file_cmd(cxx, src, dst)
    }

    fn obj_file_dst(&self, base: &r5::Utf8Path, src: &r5::Utf8Path) -> Result<r5::Utf8PathBuf, InnerError> {
        let ext = self.family.obj_file_ext();
        let src = src
            .strip_prefix(base)
            .map_err(|err| InnerError::new(InnerErrorKind::PathStripPrefix { err }))?;
        let dst = self.out_dir.join(src).with_extension(ext);
        Ok(dst)
    }

    fn obj_file_cmd(
        &self,
        src: &r5::Utf8Path,
        dst: &r5::Utf8Path,
        dep_info: &r5::DepInfo<'_>,
        bmi_dirs: &mut BTreeSet<Arc<r5::Utf8PathBuf>>,
        bmi_maps: &[(DepInfoNameYoke, Arc<r5::Utf8PathBuf>)],
    ) -> Result<Command, InnerError> {
        let cxx = self.tool.to_command();
        self.family.obj_file_cmd(cxx, src, dst, dep_info, bmi_dirs, bmi_maps)
    }

    pub(crate) fn compile_dep_file(
        &self,
        base: &r5::Utf8Path,
        path: &r5::Utf8Path,
    ) -> Result<r5::Utf8PathBuf, InnerError> {
        let src = path;
        let dst = self.dep_file_dst(base, src)?;
        let cmd = self.dep_file_cmd(src, &dst)?;
        Self::compile(self, base, src, dst, cmd)
    }

    // FIXME: where does `cc` put the object files?
    pub(crate) fn compile_obj_file(
        &self,
        dep_info: &r5::DepInfo<'_>,
        base: &r5::Utf8Path,
        path: &r5::Utf8Path,
        bmi_dirs: &mut BTreeSet<Arc<r5::Utf8PathBuf>>,
        bmi_maps: &[(DepInfoNameYoke, Arc<r5::Utf8PathBuf>)],
    ) -> Result<r5::Utf8PathBuf, InnerError> {
        let src = path;
        let dst = self.obj_file_dst(base, src)?;
        let cmd = self.obj_file_cmd(src, &dst, dep_info, bmi_dirs, bmi_maps)?;
        Self::compile(self, base, path, dst, cmd)
    }

    fn compile(
        &self,
        _root: &r5::Utf8Path, // NOTE: use for error
        _src: &r5::Utf8Path,  // NOTE: use for error
        dst: r5::Utf8PathBuf,
        mut cmd: Command,
    ) -> Result<r5::Utf8PathBuf, InnerError> {
        let output = cmd
            .output()
            .map_err(|err| InnerError::new(InnerErrorKind::CommandStatus { err }))?;
        if !output.status.success() {
            return Err(InnerError::new(InnerErrorKind::CommandCompilerNonZeroExit));
        }
        Ok(dst)
    }

    #[cfg(feature = "memchr")]
    fn module_name_is_dotted(&self, name: &str) -> bool {
        self.finders.dotted.find(name.as_bytes()).is_some()
    }

    #[cfg(not(feature = "memchr"))]
    fn module_name_is_dotted(&self, name: &str) -> bool {
        name.contains('.')
    }
}

#[derive(Clone, Copy)]
enum CompilerFamily {
    Clang,
    Gcc,
    Msvc,
}
impl CompilerFamily {
    fn bmi_file_ext(&self) -> &str {
        match self {
            CompilerFamily::Clang => "pcm",
            CompilerFamily::Gcc => "pcm",
            // https://devblogs.microsoft.com/cppblog/using-cpp-modules-in-msvc-from-the-command-line-part-1/
            CompilerFamily::Msvc => "ifc",
        }
    }

    fn dep_file_ext(&self) -> &str {
        match self {
            CompilerFamily::Clang => "ddi",
            CompilerFamily::Gcc => "ddi",
            // https://learn.microsoft.com/en-us/cpp/build/reference/scandependencies
            CompilerFamily::Msvc => "ddi",
        }
    }

    fn dep_file_cmd(&self, cxx: Command, src: &r5::Utf8Path, dst: &r5::Utf8Path) -> Result<Command, InnerError> {
        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir).map_err(|err| InnerError::new(InnerErrorKind::FsCreateDirAll { err }))?;
        }
        match self {
            CompilerFamily::Clang => self.dep_file_cmd_clang(cxx, src, dst),
            CompilerFamily::Gcc => self.dep_file_cmd_gcc(cxx, src, dst),
            CompilerFamily::Msvc => self.dep_file_cmd_msvc(cxx, src, dst),
        }
    }

    fn dep_file_cmd_clang(
        &self,
        mut cxx: Command,
        src: &r5::Utf8Path,
        dst: &r5::Utf8Path,
    ) -> Result<Command, InnerError> {
        cxx.args(["-c", src.as_str()]);
        cxx.args(["-o", dst.with_extension(self.obj_file_ext()).as_str()]);

        let mut scan_deps = Command::new("clang-scan-deps"); // FIXME: make the specific command configurable
        scan_deps.arg("-format=p1689");
        scan_deps.arg("--");
        scan_deps.arg(cxx.get_program());
        scan_deps.args(cxx.get_args());

        if let Some(dir) = cxx.get_current_dir() {
            scan_deps.current_dir(dir);
        }

        for (key, val) in cxx.get_envs().filter_map(|pair| pair.1.map(|val| (pair.0, val))) {
            scan_deps.env(key, val);
        }

        let file = std::fs::File::create(dst).map_err(|err| InnerError::new(InnerErrorKind::FileCreate { err }))?;
        let stdio = std::process::Stdio::from(file);
        scan_deps.stdout(stdio);

        Ok(scan_deps)
    }

    fn dep_file_cmd_gcc(
        &self,
        mut cxx: Command,
        src: &r5::Utf8Path,
        dst: &r5::Utf8Path,
    ) -> Result<Command, InnerError> {
        cxx.arg("-fmodules-ts");
        cxx.arg("-fdeps-format=p1689r5");
        cxx.arg(format!("-fdeps-file={}", dst.as_str()));

        cxx.arg("-E");
        cxx.arg("-MD");

        #[cfg(target_family = "unix")]
        cxx.args(["-MF", "/dev/null"]);
        #[cfg(target_family = "windows")]
        cxx.args(["-MF", "NUL"]);

        cxx.args(["-x", "c++"]);
        cxx.args(["-c", src.as_str()]);

        Ok(cxx)
    }

    fn dep_file_cmd_msvc(
        &self,
        mut cxx: Command,
        src: &r5::Utf8Path,
        dst: &r5::Utf8Path,
    ) -> Result<Command, InnerError> {
        cxx.args(["/scanDependencies", dst.as_str()]);
        // NOTE: it seems we can specify /interface even if not an interface without issue. The
        // cl.exe compiler will correctly detect and report whether the input is actually an
        // interface or not in the p1689 file. So it's easier to just unconditionally use the flag.
        cxx.args(["/interface", "/Tp"]);
        cxx.arg(src);
        Ok(cxx)
    }

    fn obj_file_ext(&self) -> &str {
        match self {
            CompilerFamily::Clang => "o",
            CompilerFamily::Gcc => "o",
            CompilerFamily::Msvc => "obj",
        }
    }

    fn obj_file_cmd(
        &self,
        cxx: Command,
        src: &r5::Utf8Path,
        dst: &r5::Utf8Path,
        dep_info: &r5::DepInfo<'_>,
        bmi_dirs: &mut BTreeSet<Arc<r5::Utf8PathBuf>>,
        bmi_maps: &[(DepInfoNameYoke, Arc<r5::Utf8PathBuf>)],
    ) -> Result<Command, InnerError> {
        let parent = if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir).map_err(|err| InnerError::new(InnerErrorKind::FsCreateDirAll { err }))?;
            Some(dir.to_path_buf())
        } else {
            None
        };
        match self {
            CompilerFamily::Clang => self.obj_file_cmd_clang(cxx, src, dst, dep_info, parent, bmi_dirs, bmi_maps),
            CompilerFamily::Gcc => self.obj_file_cmd_gcc(cxx, src, dst),
            CompilerFamily::Msvc => self.obj_file_cmd_msvc(cxx, src, dst, dep_info, parent, bmi_dirs, bmi_maps),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn obj_file_cmd_clang(
        &self,
        mut cxx: Command,
        src: &r5::Utf8Path,
        dst: &r5::Utf8Path,
        dep_info: &r5::DepInfo<'_>,
        parent: Option<r5::Utf8PathBuf>,
        bmi_dirs: &mut BTreeSet<Arc<r5::Utf8PathBuf>>,
        bmi_maps: &[(DepInfoNameYoke, Arc<r5::Utf8PathBuf>)],
    ) -> Result<Command, InnerError> {
        cxx.arg("-fmodule-output"); // TODO: support two-phase via `--precompile`

        for dir in bmi_dirs.iter() {
            cxx.arg(format!("-fprebuilt-module-path={dir}"));
        }

        for (name, path) in bmi_maps {
            let name = name.yoke.get();
            cxx.arg(format!("-fmodule-file={name}={path}"));
        }

        if dep_info
            .provides
            .first()
            .map(|provided| provided.is_interface)
            .unwrap_or(false)
        {
            cxx.args(["-x", "c++-module"]);
        } else {
            cxx.args(["-x", "c++"]);
        }
        cxx.args(["-c", src.as_str()]);
        cxx.args(["-o", dst.as_str()]);

        if let Some(dir) = parent {
            bmi_dirs.insert(Arc::new(dir));
        }

        Ok(cxx)
    }

    fn obj_file_cmd_gcc(
        &self,
        mut cxx: Command,
        src: &r5::Utf8Path,
        dst: &r5::Utf8Path,
    ) -> Result<Command, InnerError> {
        cxx.arg("-fmodules-ts");
        cxx.arg("-fdeps-format=p1689r5");

        cxx.args(["-x", "c++"]);
        cxx.args(["-c", src.as_str()]);
        cxx.args(["-o", dst.as_str()]);

        Ok(cxx)
    }

    fn obj_file_cmd_msvc(
        &self,
        mut cxx: Command,
        src: &r5::Utf8Path,
        dst: &r5::Utf8Path,
        dep_info: &r5::DepInfo<'_>,
        parent: Option<r5::Utf8PathBuf>,
        bmi_dirs: &mut BTreeSet<Arc<r5::Utf8PathBuf>>,
        bmi_maps: &[(DepInfoNameYoke, Arc<r5::Utf8PathBuf>)],
    ) -> Result<Command, InnerError> {
        cxx.arg("/c");
        cxx.args(["/Fo:", dst.as_str()]);
        cxx.args(["/ifcOutput", dst.with_extension("ifc").as_str()]);

        for dir in bmi_dirs.iter() {
            cxx.args(["/ifcSearchDir", dir.as_str()]);
        }

        for (name, path) in bmi_maps {
            let name = name.yoke.get();
            cxx.args(["/reference", &format!("{name}={path}")]);
        }

        if dep_info
            .provides
            .first()
            .map(|provided| provided.is_interface)
            .unwrap_or(false)
        {
            cxx.args(["/interface", "/Tp"]);
        }

        cxx.arg(src);

        if let Some(dir) = parent {
            bmi_dirs.insert(Arc::new(dir));
        }

        Ok(cxx)
    }
}
#[cfg(feature = "cc")]
impl TryFrom<&crate::vendor::cc::Tool> for CompilerFamily {
    type Error = InnerError;

    fn try_from(tool: &crate::vendor::cc::Tool) -> Result<Self, Self::Error> {
        if tool.is_like_gnu() {
            return Ok(CompilerFamily::Gcc);
        }
        if tool.is_like_clang() {
            return Ok(CompilerFamily::Clang);
        }
        if tool.is_like_msvc() {
            return Ok(CompilerFamily::Msvc);
        }
        Err(InnerError::new(InnerErrorKind::CompilerFamilyTryFromUnknownFamily))
    }
}

#[cfg(test)]
mod test {
    use crate::testing::BoxResult;

    #[test]
    fn compile() {
        let paths = crate::testing::corpus::src_file::items().unwrap();
        let validate = crate::testing::corpus::src_file::validate_order(paths).unwrap();
        validate.run().unwrap()
    }

    #[test]
    fn compile_reverse() -> BoxResult<()> {
        let paths = crate::testing::corpus::src_file::items()?.rev();
        let validate = crate::testing::corpus::src_file::validate_order(paths)?;
        validate.run()
    }

    #[test]
    #[should_panic]
    fn compile_incomplete() {
        fn inner() -> BoxResult<()> {
            let paths = crate::testing::corpus::src_file::items()?
                .enumerate()
                .filter_map(|(i, path)| (i != 3).then_some(path));
            let validate = crate::testing::corpus::src_file::validate_order(paths)?;
            validate.run()
        }
        inner().unwrap()
    }

    // TODO: test compiling invalid source file
}
