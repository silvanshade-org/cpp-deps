//! Versioned action identities and complete, verified local artifact records.
//!
//! A record is visible only after every immutable blob is stored. Restoration
//! stages and verifies the entire inventory before replacing destinations;
//! the caller holds its output-directory lock until all workers finish.

use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use core::time::Duration;
use std::fs;
use std::fs::File;
use std::fs::FileTimes;
use std::fs::OpenOptions;
use std::io;
use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

/// Caller attestation identifying an immutable, complete compiler installation.
///
/// The attested boundary includes drivers, subordinate tools, resource files,
/// plugins, dynamically loaded libraries, specs and configuration. Version text
/// or a digest of the compiler executable alone does not meet this obligation.
/// Changing any component requires a different identity before the next build.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct ToolchainIdentity(pub blake3::Hash);

/// Opt-in local module cache, independent of the compiler's ccache wrapper.
pub struct ModuleCache
{
    /// Local cache root, never an artifact destination.
    root: PathBuf,
    /// Explicit immutable-toolchain boundary supplied by the caller.
    toolchain: ToolchainIdentity,
}

impl ModuleCache
{
    /// Bind local storage to a complete immutable-toolchain attestation.
    ///
    /// # Specification
    /// - requires: the caller meets [`ToolchainIdentity`]'s closure obligation;
    ///   input files obey the documented cooperative-filesystem contract.
    /// - ensures: constructing a cache does not perform I/O or enable reuse
    ///   without fresh action validation.
    /// - panics: none.
    #[must_use]
    #[inline]
    pub fn new(
        root: PathBuf,
        toolchain: ToolchainIdentity,
    ) -> Self
    {
        Self { root, toolchain }
    }
    /// Return the caller-attested immutable toolchain identity.
    ///
    /// # Specification
    /// - ensures: returns the identity supplied at construction.
    /// - panics: none.
    pub(crate) fn toolchain(&self) -> ToolchainIdentity
    {
        self.toolchain
    }
}

/// Whether an invocation participates in validated reuse.
#[derive(Default)]
pub(crate) enum Policy
{
    /// Compile normally through the configured wrapper.
    #[default]
    Disabled,
    /// Validate and restore complete records from this local cache.
    Enabled(ModuleCache),
}

/// Complete compilation identity, distinct from any output's byte digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(crate) struct Action(pub blake3::Hash);

/// Content identity of exactly one stored file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
struct ArtifactDigest(blake3::Hash);

/// One unambiguously length-framed field.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct Field<'input>(pub &'input [u8]);

/// Versioned action-key builder; field order is semantically significant.
#[repr(transparent)]
pub(crate) struct Key(blake3::Hasher);

impl Default for Key
{
    fn default() -> Self
    {
        Self(blake3::Hasher::new_derive_key("cpp-deps/action/v1"))
    }
}

impl Key
{
    /// Append one field without ambiguous concatenation.
    ///
    /// # Specification
    /// - ensures: lengths and payloads are distinct, ordered little-endian
    ///   fields.
    /// - fails: a field is too large for the wire length.
    /// - panics: none.
    pub(crate) fn field(
        &mut self,
        field: Field<'_>,
    ) -> io::Result<()>
    {
        let length = u64::try_from(field.0.len()).map_err(io::Error::other)?;
        self.0.update(&length.to_le_bytes());
        self.0.update(field.0);
        Ok(())
    }

    /// Include path identity and complete file contents, size and modification
    /// time: Clang may validate embedded timestamps even for identical bytes.
    ///
    /// # Specification
    /// - ensures: content is streamed, never materialized as a complete buffer.
    /// - fails: unreadable, nonregular or concurrently changing input.
    /// - panics: none.
    pub(crate) fn file(
        &mut self,
        path: &Path,
    ) -> io::Result<()>
    {
        let (digest, size, modified) = identify(path)?;
        self.field(Field(path.as_os_str().as_encoded_bytes()))?;
        self.field(Field(digest.0.as_bytes()))?;
        self.field(Field(&size.0.to_le_bytes()))?;
        let mut time = [0_u8; 13];
        write_time(&mut time.as_mut_slice(), modified)?;
        self.field(Field(&time))
    }

    /// Finish the complete validated action key.
    ///
    /// # Specification
    /// - ensures: action and artifact digests use separate derivation domains.
    /// - panics: none.
    pub(crate) fn finish(self) -> Action
    {
        Action(self.0.finalize())
    }
}

/// Required output roles, also committed into the inventory encoding.
#[derive(Clone, Copy)]
pub(crate) enum Role
{
    /// Native object consumed by the linker.
    Object,
    /// Compiled module interface consumed by downstream compilations.
    Interface,
    /// Complete textual dependency witness.
    Depfile,
    /// Structured module dependency record.
    Structured,
    /// Compiler module-name to artifact-path mapping.
    Mapping,
    /// Additional compiler-reported output.
    Additional,
}

/// An admitted destination from the live compiler plan, never from cache data.
pub(crate) struct Destination
{
    /// Artifact role expected by the consumer.
    pub role: Role,
    /// Absolute path admitted beneath the output root.
    pub path: PathBuf,
}

/// Byte length carried by an artifact record.
#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(transparent)]
struct Size(u64);

/// Successful restoration or an explicit cache-miss reason.
pub(crate) enum Lookup
{
    /// All required destinations have been restored and checked.
    Restored,
    /// No result manifest exists for the validated action.
    Absent,
    /// A manifest or blob does not meet the live inventory contract.
    Corrupt,
}

/// Unique temporary directory removed on every normal/error exit.
#[repr(transparent)]
struct Stage(PathBuf);

/// Process-local nonce; atomic uniqueness is supplemented by create-directory.
static NONCE: AtomicU64 = AtomicU64::new(0);

impl Stage
{
    /// Reserve staging without overwriting another writer's files.
    ///
    /// # Specification
    /// - ensures: staging is on the requested filesystem and owned by this
    ///   guard.
    /// - fails: creation errors or excessive collisions with abandoned stages.
    /// - panics: none.
    fn new(parent: &Path) -> io::Result<Self>
    {
        fs::create_dir_all(parent)?;
        for _ in 0_usize .. 128_usize {
            let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".cpp-deps-{}-{nonce}", std::process::id()));
            // Exclusive creation is required here; create_dir_all cannot reserve.
            #[expect(
                clippy::create_dir,
                reason = "staging reservation must reject an existing directory"
            )]
            match fs::create_dir(&path) {
                | Ok(()) => return Ok(Self(path)),
                | Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
                | Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot reserve cache staging directory",
        ))
    }
}

impl Drop for Stage
{
    fn drop(&mut self)
    {
        // Staging is never a published record; cleanup cannot turn a failed
        // operation into a hit, including after process interruption.
        drop(fs::remove_dir_all(&self.0));
    }
}

/// Stream and identify a regular file while checking observable read stability.
///
/// # Specification
/// - requires: cooperative filesystem; metadata equality is not a snapshot
///   proof.
/// - ensures: the byte digest identifies exactly the streamed contents.
/// - fails: nonregular file, read failure, or metadata change while hashing.
/// - panics: none.
fn identify(path: &Path) -> io::Result<(ArtifactDigest, Size, SystemTime)>
{
    let file = File::open(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact is not a regular file",
        ));
    }
    let modified = before.modified()?;
    let mut hash = blake3::Hasher::new_derive_key("cpp-deps/artifact/v1");
    hash.update_reader(&file)?;
    let after = file.metadata()?;
    if before.len() != after.len() || modified != after.modified()? || hash.count() != before.len()
    {
        return Err(io::Error::other("file changed during content validation"));
    }
    Ok((
        ArtifactDigest(hash.finalize()),
        Size(before.len()),
        modified,
    ))
}

/// Encode modification times on either side of the Unix epoch.
///
/// # Specification
/// - ensures: sign, seconds and nanoseconds round-trip without loss.
/// - fails: output write failure.
/// - panics: none.
fn write_time<Output>(
    output: &mut Output,
    time: SystemTime,
) -> io::Result<()>
where
    Output: io::Write,
{
    let (sign, duration) = match time.duration_since(SystemTime::UNIX_EPOCH) {
        | Ok(duration) => (0_u8, duration),
        | Err(error) => (1_u8, error.duration()),
    };
    output.write_all(&[sign])?;
    output.write_all(&duration.as_secs().to_le_bytes())?;
    output.write_all(&duration.subsec_nanos().to_le_bytes())
}

/// Decode a bounded timestamp with checked epoch arithmetic.
///
/// # Specification
/// - ensures: invalid signs and nanoseconds are rejected.
/// - fails: truncation, malformed time or platform time overflow.
/// - panics: none.
fn read_time(input: &mut Field<'_>) -> io::Result<SystemTime>
{
    let mut sign = [0_u8; 1];
    let mut seconds = [0_u8; 8];
    let mut nanos = [0_u8; 4];
    input.0.read_exact(&mut sign)?;
    input.0.read_exact(&mut seconds)?;
    input.0.read_exact(&mut nanos)?;
    let nanos = u32::from_le_bytes(nanos);
    if nanos >= 1_000_000_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid timestamp",
        ));
    }
    let duration = Duration::new(u64::from_le_bytes(seconds), nanos);
    let time = match sign {
        | [0] => SystemTime::UNIX_EPOCH.checked_add(duration),
        | [1] => SystemTime::UNIX_EPOCH.checked_sub(duration),
        | _ => None,
    };
    time.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "timestamp outside platform range",
        )
    })
}

/// Manifest domain marker; schema changes require a new domain and marker.
const MAGIC: &[u8] = b"cpp-deps/result/v1\0";

/// Append the live destination binding, including its role and path length.
///
/// # Specification
/// - ensures: arbitrary cache bytes cannot redirect materialization.
/// - fails: unrepresentable length or output write failure.
/// - panics: none.
fn write_destination(
    output: &mut Vec<u8>,
    destination: &Destination,
) -> io::Result<()>
{
    let role = match destination.role {
        | Role::Object => 0_u8,
        | Role::Interface => 1,
        | Role::Depfile => 2,
        | Role::Structured => 3,
        | Role::Mapping => 4,
        | Role::Additional => 5,
    };
    output.write_all(&[role])?;
    let bytes = destination.path.as_os_str().as_encoded_bytes();
    let length = u64::try_from(bytes.len()).map_err(io::Error::other)?;
    output.write_all(&length.to_le_bytes())?;
    output.write_all(bytes)
}

/// Verify a fixed expected prefix without allocating from untrusted lengths.
///
/// # Specification
/// - ensures: comparison is bounded by the live plan's encoded size.
/// - fails: truncation or mismatched prefix.
/// - panics: none.
fn consume(
    input: &mut Field<'_>,
    expected: Field<'_>,
) -> io::Result<()>
{
    let prefix = input
        .0
        .get(.. expected.0.len())
        .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
    if prefix != expected.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache inventory mismatch",
        ));
    }
    input.0 = input
        .0
        .get(expected.0.len() ..)
        .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
    Ok(())
}

impl ModuleCache
{
    /// Publish immutable blobs, then atomically select one coherent result set.
    ///
    /// # Specification
    /// - requires: all admitted outputs exist and input stability was
    ///   validated; the caller exclusively owns output materialization for this
    ///   session.
    /// - ensures: a manifest never names an incompletely written blob.
    /// - fails: read, write, synchronization or atomic publication failures.
    /// - panics: none.
    pub(crate) fn publish(
        &self,
        action: Action,
        destinations: &[Destination],
    ) -> io::Result<()>
    {
        let blobs = self.root.join("blobs");
        let results = self.root.join("results");
        fs::create_dir_all(&blobs)?;
        fs::create_dir_all(&results)?;
        let stage = Stage::new(&self.root)?;
        let mut record = Vec::new();
        record.extend_from_slice(MAGIC);
        record.extend_from_slice(action.0.as_bytes());
        let count = u64::try_from(destinations.len()).map_err(io::Error::other)?;
        record.extend_from_slice(&count.to_le_bytes());
        for (index, destination) in destinations.iter().enumerate() {
            let source = &destination.path;
            let modified = fs::metadata(source)?.modified()?;
            let staged = stage.0.join(index.to_string());
            fs::copy(source, &staged)?;
            let (digest, size, _) = identify(&staged)?;
            File::open(&staged)?.sync_all()?;
            fs::rename(staged, blobs.join(digest.0.to_hex().as_str()))?;
            write_destination(&mut record, destination)?;
            record.extend_from_slice(digest.0.as_bytes());
            record.extend_from_slice(&size.0.to_le_bytes());
            write_time(&mut record, modified)?;
        }
        let mut hash = blake3::Hasher::new_derive_key("cpp-deps/manifest/v1");
        hash.update(&record);
        record.extend_from_slice(hash.finalize().as_bytes());
        let manifest = stage.0.join("manifest");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest)?;
        file.write_all(&record)?;
        file.sync_all()?;
        #[cfg(unix)]
        File::open(&blobs)?.sync_all()?;
        fs::rename(manifest, results.join(action.0.to_hex().as_str()))?;
        #[cfg(unix)]
        File::open(results)?.sync_all()?;
        Ok(())
    }

    /// Restore a complete result, with all sizes and digests checked first.
    ///
    /// # Specification
    /// - requires: output lock remains held until the entire build finishes;
    ///   destinations were admitted from the live compiler plan.
    /// - ensures: a corrupt entry releases no consumer; timestamps survive
    ///   restoration to meet compiler-embedded file validation.
    /// - fails: destination creation or replacement failures.
    /// - panics: none.
    ///
    /// # Adequacy
    /// - hypothesis: one missing or same-size corrupt companion cannot replace
    ///   any live output; role/path/count substitutions cannot redirect writes.
    /// - witness: `tests::incomplete_record_never_replaces_live_outputs` and
    ///   `tests::live_inventory_rejects_role_path_and_count_substitution`.
    pub(crate) fn restore(
        &self,
        action: Action,
        destinations: &[Destination],
        output_root: &Path,
    ) -> io::Result<Lookup>
    {
        let manifest = self.root.join("results").join(action.0.to_hex().as_str());
        let file = match File::open(&manifest) {
            | Ok(file) => file,
            | Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Lookup::Absent),
            | Err(error) => return Err(error),
        };
        // The bound comes only from trusted expected destinations, never from
        // count/length fields supplied by a corrupt manifest.
        let mut expected = Vec::new();
        expected.extend_from_slice(MAGIC);
        expected.extend_from_slice(action.0.as_bytes());
        let count = u64::try_from(destinations.len()).map_err(io::Error::other)?;
        expected.extend_from_slice(&count.to_le_bytes());
        let mut bound = expected
            .len()
            .checked_add(32)
            .ok_or_else(|| io::Error::other("manifest size overflow"))?;
        for destination in destinations {
            bound = bound
                .checked_add(destination.path.as_os_str().as_encoded_bytes().len())
                .and_then(|size| size.checked_add(9))
                .and_then(|size| size.checked_add(53))
                .ok_or_else(|| io::Error::other("manifest size overflow"))?;
        }
        let limit = u64::try_from(bound).map_err(io::Error::other)?;
        if file.metadata()?.len() != limit {
            return Ok(Lookup::Corrupt);
        }
        let mut record = Vec::with_capacity(bound);
        file.take(limit).read_to_end(&mut record)?;
        let payload_length = bound
            .checked_sub(32)
            .ok_or_else(|| io::Error::other("manifest size underflow"))?;
        let Some((payload, checksum)) = record.split_at_checked(payload_length)
        else {
            return Ok(Lookup::Corrupt);
        };
        let mut hash = blake3::Hasher::new_derive_key("cpp-deps/manifest/v1");
        hash.update(payload);
        if hash.finalize().as_bytes() != checksum {
            return Ok(Lookup::Corrupt);
        }
        let stage = Stage::new(output_root)?;
        match self.stage_restore(
            action,
            destinations,
            &stage,
            Field(payload),
            Field(&expected),
        ) {
            | Ok(()) => {},
            | Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidData
                        | io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::NotFound
                ) =>
            {
                return Ok(Lookup::Corrupt);
            },
            | Err(error) => return Err(error),
        }
        for (index, destination) in destinations.iter().enumerate() {
            if let Some(parent) = destination.path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(stage.0.join(index.to_string()), &destination.path)?;
        }
        Ok(Lookup::Restored)
    }

    /// Validate and stage every member without exposing any destination.
    ///
    /// # Specification
    /// - ensures: only live-plan paths are used for replacement; records carry
    ///   exactly one digest/size/time tuple for every expected destination.
    /// - fails: malformed records, missing/corrupt blobs or stage write errors.
    /// - panics: none.
    fn stage_restore(
        &self,
        _action: Action,
        destinations: &[Destination],
        stage: &Stage,
        payload: Field<'_>,
        expected: Field<'_>,
    ) -> io::Result<()>
    {
        let mut input = payload;
        consume(&mut input, expected)?;
        let mut binding = Vec::new();
        for (index, destination) in destinations.iter().enumerate() {
            binding.clear();
            write_destination(&mut binding, destination)?;
            consume(&mut input, Field(&binding))?;
            let mut digest = [0_u8; 32];
            let mut size = [0_u8; 8];
            input.0.read_exact(&mut digest)?;
            input.0.read_exact(&mut size)?;
            let modified = read_time(&mut input)?;
            let digest = ArtifactDigest(blake3::Hash::from_bytes(digest));
            let source = self.root.join("blobs").join(digest.0.to_hex().as_str());
            let staged = stage.0.join(index.to_string());
            fs::copy(source, &staged)?;
            let (actual, length, _) = identify(&staged)?;
            if actual != digest || length != Size(u64::from_le_bytes(size)) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "cache artifact digest or size mismatch",
                ));
            }
            let file = OpenOptions::new().write(true).open(&staged)?;
            file.set_times(FileTimes::new().set_modified(modified))?;
        }
        if !input.0.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trailing manifest bytes",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
