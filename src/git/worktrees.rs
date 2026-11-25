//! Lists a repository's worktrees and adds one for a task.
//!
//! Everything here reads, except `add`. That is the one Git command Milo
//! runs that changes a repository, and it runs only when a person starts a
//! task on a branch that has no worktree yet. It never touches the checkout
//! it was asked from: the new branch and its files go in a folder of their own.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use super::run;

/// More worktrees than this are not listed; a picker could not show them.
const LIST_LIMIT: u64 = 4 << 20;

/// One checkout of a repository: its main folder or a linked worktree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Worktree {
    pub path: PathBuf,
    /// The branch checked out there, or none when it is at a commit of no branch.
    pub branch: Option<String>,
}

/// The repository's worktrees that have files: a bare repository's own
/// folder and a worktree whose folder is gone are left out.
pub(crate) fn list(root: &Path) -> Option<Vec<Worktree>> {
    let raw = run::limited_output(root, &[], &["worktree", "list", "--porcelain", "-z"], LIST_LIMIT).ok()?;
    Some(parse(&raw))
}

/// Records end with an empty field, and each field is a word and its value.
fn parse(raw: &[u8]) -> Vec<Worktree> {
    let mut found = Vec::new();
    let (mut path, mut branch, mut unusable) = (None, None, false);
    for field in raw.split(|&b| b == 0) {
        if field.is_empty() {
            if let Some(path) = path.take().filter(|_| !unusable) {
                found.push(Worktree { path, branch: branch.take() });
            }
            (branch, unusable) = (None, false);
        } else if let Some(folder) = field.strip_prefix(b"worktree ") {
            path = Some(PathBuf::from(OsStr::from_bytes(folder)));
        } else if let Some(name) = field.strip_prefix(b"branch refs/heads/") {
            branch = String::from_utf8(name.to_vec()).ok();
        } else if field == b"bare" || field.starts_with(b"prunable") {
            unusable = true;
        }
    }
    found
}

/// The full ID of the commit `revision` names, or none when it names no commit.
/// The outer `None` means Git could not say.
pub(crate) fn commit(root: &Path, revision: &str) -> Option<Option<String>> {
    let name = format!("{revision}^{{commit}}");
    run::optional_oid(root, &[], &["rev-parse", "--verify", "--quiet", "--end-of-options", &name])
}

/// Whether the repository has a local branch of this name. `None` means Git could not say.
pub(crate) fn has_branch(root: &Path, branch: &str) -> Option<bool> {
    let name = format!("refs/heads/{branch}");
    run::optional_oid(root, &[], &["rev-parse", "--verify", "--quiet", "--end-of-options", &name])
        .map(|id| id.is_some())
}

/// Whether Git would take `name` as a branch name as written. A shorthand
/// such as `@{-1}`, which Git would turn into another name, is not one.
pub(crate) fn branch_name(root: &Path, name: &str) -> bool {
    !name.starts_with('-')
        && run::limited_output(root, &[], &["check-ref-format", "--branch", name], 4096)
            .is_ok_and(|said| said.strip_suffix(b"\n") == Some(name.as_bytes()))
}

/// Local branches by name, most recently committed first, up to a number a picker can hold.
pub(crate) fn branches(root: &Path) -> Option<Vec<String>> {
    let args = ["for-each-ref", "--count=500", "--sort=-committerdate", "--format=%(refname:short)", "refs/heads/"];
    let raw = String::from_utf8(run::limited_output(root, &[], &args, LIST_LIMIT).ok()?).ok()?;
    Some(raw.lines().map(str::to_owned).collect())
}

/// The branch the remote `origin` treats as its main one, as `origin/NAME`, when the clone recorded it.
pub(crate) fn remote_default(root: &Path) -> Option<String> {
    let raw = run::limited_output(root, &[], &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"], 4096);
    let name = String::from_utf8(raw.ok()?).ok()?;
    let name = name.strip_suffix('\n')?;
    (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| name.to_owned())
}

/// Where a task's worktree goes when no folder is named: beside the
/// repository's main folder, in `PROJECT-worktrees`, under the branch's name
/// with each `/` as a `-`. From a linked worktree this is still beside the
/// main folder, so every task of one project lands in the same place.
pub(crate) fn default_path(root: &Path, branch: &str) -> Option<PathBuf> {
    let common = super::git_dirs(root).ok()??.common?;
    // A repository that holds its own files keeps its records in `.git`; a
    // bare one is its own folder, usually named `PROJECT.git`.
    let main = if common.file_name() == Some(OsStr::new(".git")) { common.parent()? } else { &common };
    let project = main.file_name()?.to_str()?;
    let project = project.strip_suffix(".git").filter(|name| !name.is_empty()).unwrap_or(project);
    Some(main.parent()?.join(format!("{project}-worktrees")).join(branch.replace('/', "-")))
}

/// What a new worktree checks out.
pub(crate) enum Start<'a> {
    /// A new branch of this name, starting at the commit with this full ID.
    NewBranch { branch: &'a str, commit: &'a str },
    /// A branch that exists and is checked out nowhere.
    Branch(&'a str),
}

/// Adds a worktree at `path`, or says why Git would not in Git's own words.
pub(crate) fn add(root: &Path, path: &Path, start: &Start<'_>) -> Result<(), String> {
    let mut args: Vec<&OsStr> = vec![OsStr::new("worktree"), OsStr::new("add")];
    match start {
        Start::NewBranch { branch, commit } => {
            args.extend([OsStr::new("-b"), OsStr::new(branch), OsStr::new("--"), path.as_os_str(), OsStr::new(commit)]);
        }
        Start::Branch(branch) => args.extend([OsStr::new("--"), path.as_os_str(), OsStr::new(branch)]),
    }
    run::change(root, &args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn a_listing_keeps_only_worktrees_with_files_and_their_branches() {
        let raw = b"worktree /repo.git\0bare\0\0worktree /work/main\0HEAD 1\0branch refs/heads/main\0\0\
            worktree /work/at a commit\0HEAD 2\0detached\0\0\
            worktree /gone\0HEAD 3\0branch refs/heads/old\0prunable gitdir file points to non-existent location\0\0\
            worktree /work/topic\0HEAD 4\0branch refs/heads/feature/topic\0locked\0\0";
        let listed = parse(raw);
        let expected =
            [("/work/main", Some("main")), ("/work/at a commit", None), ("/work/topic", Some("feature/topic"))];
        assert_eq!(listed.len(), expected.len());
        for (found, (path, branch)) in listed.iter().zip(expected) {
            assert_eq!((found.path.as_path(), found.branch.as_deref()), (Path::new(path), branch));
        }
    }

    #[test]
    fn a_new_worktree_goes_beside_the_main_folder_from_any_worktree_and_leaves_the_caller_alone() {
        let f = Fixture::new();
        f.write("page.txt", "one\n");
        f.commit("first");
        let home = f.root.parent().unwrap();
        let first = default_path(&f.root, "feature/one").unwrap();
        assert_eq!(first, home.join("project-worktrees/feature-one"));

        let commit = commit(&f.root, "main").unwrap().unwrap();
        add(&f.root, &first, &Start::NewBranch { branch: "feature/one", commit: &commit }).unwrap();
        assert_eq!(std::fs::read_to_string(first.join("page.txt")).unwrap(), "one\n");
        // The checkout that asked is still on its branch, and the new one is listed beside it.
        let listed = list(&f.root).unwrap();
        assert_eq!(listed[0].branch.as_deref(), Some("main"));
        assert!(listed.iter().any(|tree| tree.path == first && tree.branch.as_deref() == Some("feature/one")));
        assert_eq!(default_path(&first, "two").unwrap(), home.join("project-worktrees/two"));

        // Git's own reason comes back when it refuses, here a branch that is already checked out.
        let again = add(&f.root, &home.join("elsewhere"), &Start::Branch("feature/one")).unwrap_err();
        assert!(again.contains("feature/one"), "{again}");
        assert!(!home.join("elsewhere").exists());
    }

    #[test]
    fn revisions_and_branch_names_are_checked_without_being_read_as_options() {
        let f = Fixture::new();
        f.write("page.txt", "one\n");
        f.commit("first");
        assert_eq!(commit(&f.root, "no-such-branch"), Some(None));
        assert_eq!(commit(&f.root, "--all"), Some(None));
        assert_eq!(has_branch(&f.root, "main"), Some(true));
        assert_eq!(has_branch(&f.root, "other"), Some(false));
        f.git(&["branch", "other"]);
        let mut branches = branches(&f.root).unwrap();
        branches.sort();
        assert_eq!(branches, ["main", "other"]);
        assert_eq!(remote_default(&f.root), None);
        f.git(&["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        f.git(&["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/trunk"]);
        assert_eq!(remote_default(&f.root).as_deref(), Some("origin/trunk"));
        assert!(branch_name(&f.root, "feature/one"));
        for refused in ["-b", "@{-1}", "two words", "ends.lock", ""] {
            assert!(!branch_name(&f.root, refused), "{refused:?}");
        }
    }
}
