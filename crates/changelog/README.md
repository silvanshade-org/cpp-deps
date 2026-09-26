# cpp-deps-changelog

Repository tooling invoked by `mise run changelog:render` through treefmt. It uses Git tags and `git-cliff` to render `CHANGELOG.md`. On a review branch it replaces its transient commits with the open PR's eventual squash subject; on `main` it renders landed history directly. A missing PR or stale base fails instead of publishing a guessed changelog.
