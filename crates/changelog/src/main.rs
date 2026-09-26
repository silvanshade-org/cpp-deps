//! Generate the root changelog for either landed history or a review's squash
//! subject.

use std::env;
use std::fs;
use std::io;
use std::process::Command;

use serde::Deserialize;

/// Whether Git history already contains landed commits or an unlanded review.
enum RenderMode
{
    /// Landed default-branch or merge-group history.
    Main,
    /// A review whose commits will become one squash commit.
    Review(PullRequest),
}

/// The title, number, and base of an open review.
struct PullRequest
{
    /// Number appended to the eventual squash subject.
    number: u64,
    /// Current PR title.
    title: String,
    /// Base revision that must precede this checkout.
    base_sha: String,
}

/// Pull-request fields in GitHub's Actions event payload.
#[derive(Deserialize)]
struct Event
{
    /// Review metadata.
    pull_request: EventPullRequest,
}

/// Review metadata in the Actions event payload.
#[derive(Deserialize)]
struct EventPullRequest
{
    /// Review number.
    number: u64,
    /// Event-time review title.
    title: String,
    /// Event-time base revision.
    base: EventBase,
}

/// Review base in the Actions event payload.
#[derive(Deserialize)]
struct EventBase
{
    /// Base commit hash.
    sha: String,
}

/// Review metadata returned by GitHub CLI on a local branch.
#[derive(Deserialize)]
struct LocalPullRequest
{
    /// Review number.
    number: u64,
    /// Live review title.
    title: String,
    /// Branch whose current head is the base.
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
}

/// Capture successful command output or return its stderr and status.
///
/// # Specification
/// - provides: stdout bytes only for a successful process exit.
/// - fails: process launch failures and nonzero exits retain the command and
///   stderr.
/// - panics: none.
///
/// # Adequacy
/// - hypothesis: the Git fixture rejects failed metadata commands rather than
///   rendering guessed notes.
/// - witness: `tests::review_and_landed_subjects_render_identically`
fn output(command: &mut Command) -> io::Result<Vec<u8>>
{
    let result = command.output()?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "{} failed ({}): {}",
            command.get_program().to_string_lossy(),
            result.status,
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(result.stdout)
}

/// Run a command that writes its own output and preserve its failure status.
///
/// # Specification
/// - ensures: a successful exit is the only accepted outcome.
/// - fails: reports launch errors or a nonzero exit.
/// - panics: none.
///
/// # Adequacy
/// - hypothesis: the Git fixture rejects an invalid git-cliff policy without
///   updating output.
/// - witness: `tests::review_and_landed_subjects_render_identically`
fn run(command: &mut Command) -> io::Result<()>
{
    let result = command.status()?;
    if !result.success() {
        return Err(io::Error::other(format!(
            "{} failed ({result})",
            command.get_program().to_string_lossy()
        )));
    }
    Ok(())
}

/// Select GitHub's event review or the current local branch's open review.
///
/// # Specification
/// - provides: a landed history on main/merge-group/push; otherwise an exact PR
///   base and title.
/// - fails: missing PR metadata, invalid JSON, absent base, or Git/GitHub CLI
///   failure.
/// - panics: none.
///
/// # Adequacy
/// - hypothesis: a missing review cannot silently render branch-only commits as
///   released history.
/// - witness: `tests::review_and_landed_subjects_render_identically`
fn render_mode() -> io::Result<RenderMode>
{
    match env::var("GITHUB_EVENT_NAME") {
        | Ok(event) if event == "pull_request" => {
            let event_path = env::var_os("GITHUB_EVENT_PATH")
                .ok_or_else(|| io::Error::other("pull request has no event payload path"))?;
            let event: Event = serde_json::from_slice(&fs::read(event_path)?)?;
            let review = event.pull_request;
            Ok(RenderMode::Review(PullRequest {
                number: review.number,
                title: review.title,
                base_sha: review.base.sha,
            }))
        },
        | Ok(event) if event == "push" || event == "merge_group" => Ok(RenderMode::Main),
        | _ => {
            let mut branch = Command::new("git");
            branch.args(["branch", "--show-current"]);
            let branch = output(&mut branch)?;
            if core::str::from_utf8(&branch)
                .map_err(io::Error::other)?
                .trim()
                == "main"
            {
                return Ok(RenderMode::Main);
            }
            let mut view = Command::new("gh");
            view.args(["pr", "view", "--json", "number,title,baseRefName"]);
            let review: LocalPullRequest = serde_json::from_slice(&output(&mut view)?)?;
            let mut base = Command::new("git");
            base.args([
                "ls-remote",
                "origin",
                &format!("refs/heads/{}", review.base_ref_name),
            ]);
            let base = output(&mut base)?;
            let base_sha = core::str::from_utf8(&base)
                .map_err(io::Error::other)?
                .split_once('\t')
                .map(|(sha, _)| sha.to_owned())
                .ok_or_else(|| {
                    io::Error::other(format!("missing origin branch {}", review.base_ref_name))
                })?;
            Ok(RenderMode::Review(PullRequest {
                number: review.number,
                title: review.title,
                base_sha,
            }))
        },
    }
}

/// Render stable tags plus either current landed history or a synthetic squash
/// subject.
///
/// # Specification
/// - requires: version tags and the PR base commit are available in the
///   checkout.
/// - ensures: review-only commits are skipped, with one final squash title
///   instead.
/// - fails: stale base, Git, git-cliff, or Markdown formatting failures leave
///   the changelog untouched.
/// - panics: none.
///
/// # Adequacy
/// - hypothesis: the review and its landed squash yield byte-identical notes.
/// - witness: `tests::review_and_landed_subjects_render_identically`
fn render(mode: &RenderMode) -> io::Result<()>
{
    let mut cliff = Command::new("git-cliff");
    cliff.args([
        "--config",
        "cliff.toml",
        "--offline",
        "--output",
        ".git-cliff-output",
        "--exclude-path",
        "CHANGELOG.md",
    ]);
    if let RenderMode::Review(ref review) = *mode {
        let mut ancestor = Command::new("git");
        ancestor.args(["merge-base", "--is-ancestor", &review.base_sha, "HEAD"]);
        let result = ancestor.status()?;
        if !result.success() {
            return Err(io::Error::other(format!(
                "review base {} is not an ancestor of HEAD ({result})",
                review.base_sha
            )));
        }
        let mut revisions = Command::new("git");
        revisions.args(["rev-list", &format!("{}..HEAD", review.base_sha)]);
        let revisions = output(&mut revisions)?;
        for revision in core::str::from_utf8(&revisions)
            .map_err(io::Error::other)?
            .lines()
        {
            cliff.args(["--skip-commit", revision]);
        }
        cliff.args([
            "--with-commit",
            &format!("{} (#{})", review.title, review.number),
        ]);
    }
    run(&mut cliff)?;
    run(Command::new("rumdl").args(["fmt", ".git-cliff-output"]))?;
    Ok(())
}

/// Replace the changelog only when rendered bytes differ.
///
/// # Specification
/// - ensures: a current destination retains its content and metadata.
/// - fails: unexpected I/O errors are returned without silently dropping
///   output.
/// - panics: none.
///
/// # Adequacy
/// - hypothesis: regeneration with unchanged history does not rewrite the
///   destination.
/// - witness: `tests::review_and_landed_subjects_render_identically`
fn install() -> io::Result<()>
{
    let candidate = fs::read(".git-cliff-output")?;
    match fs::read("CHANGELOG.md") {
        | Ok(current) if current == candidate => {
            fs::remove_file(".git-cliff-output")?;
            return Ok(());
        },
        | Ok(_) => {},
        | Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        | Err(error) => return Err(error),
    }
    fs::rename(".git-cliff-output", "CHANGELOG.md")?;
    Ok(())
}

/// Generate the current root release history without modifying a current file.
///
/// # Specification
/// - requires: pinned tools and Git tags are available in this repository.
/// - ensures: review output matches the eventual squash subject and main uses
///   landed history.
/// - fails: metadata, rendering, and I/O errors are propagated.
/// - panics: none.
///
/// # Adequacy
/// - hypothesis: a synthetic review commit produces the same bytes as a landed
///   squash.
/// - witness: `tests::review_and_landed_subjects_render_identically`
fn main() -> io::Result<()>
{
    let mode = render_mode()?;
    render(&mode)?;
    install()?;
    Ok(())
}
