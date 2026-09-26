//! Isolated Git history witness for review-to-squash release-note rendering.

#[cfg(test)]
mod tests
{
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    #[test]
    fn review_and_landed_subjects_render_identically()
    {
        let sequence = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time must follow the Unix epoch")
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "cpp-deps-changelog-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create isolated Git fixture");
        fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cliff.toml"),
            root.join("cliff.toml"),
        )
        .expect("use the project release-note policy");
        let git = |args: &[&str]| -> String {
            let result = Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("run Git");
            assert!(
                result.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            String::from_utf8(result.stdout).expect("Git emits UTF-8")
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Fixture"]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        git(&["add", "cliff.toml"]);
        git(&["commit", "-m", "chore(repo): initial release"]);
        git(&["tag", "v0.1.0"]);
        let base = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-b", "review"]);
        fs::write(root.join("feature.txt"), "first\n").expect("write first change");
        git(&["add", "feature.txt"]);
        git(&["commit", "-m", "feat(core): first transient commit"]);
        fs::write(root.join("feature.txt"), "complete\n").expect("write next change");
        git(&["commit", "-am", "fix(core): second transient commit"]);
        let event = root.join("review.json");
        fs::write(
            &event,
            format!(
                "{{\"pull_request\":{{\"number\":87,\"title\":\"feat(core): compile modules\",\"base\":{{\"sha\":\"{}\"}}}}}}",
                base.trim()
            ),
        )
        .expect("write PR metadata");
        let renderer = env!("CARGO_BIN_EXE_cpp-deps-changelog");
        let review = Command::new(renderer)
            .current_dir(&root)
            .env("GITHUB_EVENT_NAME", "pull_request")
            .env("GITHUB_EVENT_PATH", &event)
            .status()
            .expect("render review changelog");
        assert!(review.success(), "review render must succeed");
        let candidate = fs::read(root.join("CHANGELOG.md")).expect("review changelog");
        let notes = String::from_utf8(candidate.clone()).expect("Markdown UTF-8");
        assert!(
            notes.contains("Compile modules (#87)"),
            "final review subject appears"
        );
        assert!(
            !notes.contains("transient commit"),
            "intermediate subjects must not leak"
        );
        let marker = UNIX_EPOCH;
        fs::File::open(root.join("CHANGELOG.md"))
            .expect("open generated changelog")
            .set_times(fs::FileTimes::new().set_modified(marker))
            .expect("set stable changelog timestamp");
        let unchanged = Command::new(renderer)
            .current_dir(&root)
            .env("GITHUB_EVENT_NAME", "pull_request")
            .env("GITHUB_EVENT_PATH", &event)
            .status()
            .expect("rerender unchanged review");
        assert!(unchanged.success(), "unchanged history still renders");
        assert_eq!(
            fs::metadata(root.join("CHANGELOG.md"))
                .expect("generated changelog metadata")
                .modified()
                .expect("modification time"),
            marker,
            "unchanged history must not rewrite the changelog"
        );

        let missing = Command::new(renderer)
            .current_dir(&root)
            .env_remove("GITHUB_EVENT_NAME")
            .env_remove("GITHUB_EVENT_PATH")
            .output()
            .expect("run review without a remote PR");
        assert!(
            !missing.status.success(),
            "missing review metadata must fail"
        );
        assert_eq!(
            fs::read(root.join("CHANGELOG.md")).expect("preserved notes"),
            candidate
        );

        fs::write(
            root.join("cliff.toml"),
            "[broken
",
        )
        .expect("invalidate release-note policy");
        let invalid = Command::new(renderer)
            .current_dir(&root)
            .env("GITHUB_EVENT_NAME", "pull_request")
            .env("GITHUB_EVENT_PATH", &event)
            .output()
            .expect("run renderer with invalid policy");
        assert!(
            !invalid.status.success(),
            "invalid release-note policy must fail"
        );
        assert_eq!(
            fs::read(root.join("CHANGELOG.md")).expect("preserved notes"),
            candidate
        );
        fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cliff.toml"),
            root.join("cliff.toml"),
        )
        .expect("restore release-note policy");

        fs::write(&event, "{\"pull_request\":{\"number\":87,\"title\":\"feat(core): compile modules\",\"base\":{\"sha\":\"0000000000000000000000000000000000000000\"}}}")
            .expect("write stale base");
        let stale = Command::new(renderer)
            .current_dir(&root)
            .env("GITHUB_EVENT_NAME", "pull_request")
            .env("GITHUB_EVENT_PATH", &event)
            .output()
            .expect("run stale-base render");
        assert!(!stale.status.success(), "a stale base must be rejected");
        assert_eq!(
            fs::read(root.join("CHANGELOG.md")).expect("preserved notes"),
            candidate
        );

        git(&["checkout", "main"]);
        fs::write(root.join("feature.txt"), "complete\n").expect("write landed feature");
        git(&["add", "feature.txt"]);
        git(&["commit", "-m", "feat(core): compile modules (#87)"]);
        let landed = Command::new(renderer)
            .current_dir(&root)
            .env("GITHUB_EVENT_NAME", "push")
            .status()
            .expect("render landed changelog");
        assert!(landed.success(), "landed render must succeed");
        assert_eq!(
            fs::read(root.join("CHANGELOG.md")).expect("landed notes"),
            candidate
        );
        fs::remove_dir_all(&root).expect("remove isolated Git fixture");
    }
}
