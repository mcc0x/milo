//! Remembers which files a reader had open as tabs, for one worktree at a time.
//!
//! A *list* is the tabs of a reader: for each, the file's path in the
//! worktree, the commit if it was read as of one, and the line at the top of
//! the view; and which tab was shown. The *last tabs* are the list of the
//! reader most recently used in a worktree, kept up to date as that reader's
//! tabs open and close. A *named set* is a list someone saved under a name,
//! and changes only when they replace, rename or delete it.
//!
//! Lists live in milo's state folder, in a folder named after the worktree's
//! path. They never hold a file's text or a draft.

use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

use super::rescue::{hashed, private_dir};
use crate::fs_ops::{self, Temp};
use crate::git::CommitId;

const HEADER: &str = "milo tabs 1";
/// The most tabs a list keeps, and the most bytes of a list that are read.
const MOST_TABS: usize = 200;
const LIMIT: u64 = 1 << 20;
/// The most characters in a set's name.
pub(super) const NAME_ROOM: usize = 48;

/// One tab of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    /// The file's path from the top of the worktree, written with `/`.
    pub(super) path: String,
    /// The full ID of the commit the file was read at; none for the working file.
    pub(super) commit: Option<String>,
    /// The file line at the top of the view, counted from zero.
    pub(super) line: usize,
}

impl Entry {
    /// Whether this is the same file at the same version as `other`, wherever each was scrolled to.
    pub(super) fn same_file(&self, other: &Self) -> bool {
        self.path == other.path && self.commit == other.commit
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct List {
    pub(super) entries: Vec<Entry>,
    /// The tab that was shown, as a position in `entries`.
    pub(super) active: usize,
}

/// A named set as it was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Named {
    pub(super) name: String,
    pub(super) list: List,
}

/// Why a named set was not saved or renamed.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// Another set has the name. Replacing it is a separate, explicit request.
    Taken,
    Said(&'static str),
}

/// What to do when the name a set is saved under already has one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Collision {
    Refuse,
    Replace,
}

/// A name as it is kept: trimmed, one line, of a length a row can show.
pub(super) fn clean_name(name: &str) -> Result<&str, Refusal> {
    let name = name.trim();
    if name.is_empty() {
        Err(Refusal::Said("Give the set a name."))
    } else if name.chars().count() > NAME_ROOM || name.chars().any(char::is_control) {
        Err(Refusal::Said("That name cannot be used."))
    } else {
        Ok(name)
    }
}

/// Where one worktree's lists are kept.
pub(super) struct Shelf {
    state: PathBuf,
    /// The folder under the state folder, named after the worktree's path.
    folder: String,
}

impl Shelf {
    pub(super) fn new(state: PathBuf, root: &Path) -> Self {
        Self { state, folder: format!("tabs/{}", hashed(root.as_os_str().as_encoded_bytes())) }
    }

    fn dir(&self) -> PathBuf {
        self.state.join(&self.folder)
    }

    fn last_path(&self) -> PathBuf {
        self.dir().join("last.tabs")
    }

    fn set_path(&self, name: &str) -> PathBuf {
        self.dir().join(format!("set-{}.tabs", hashed(name.as_bytes())))
    }

    pub(super) fn last(&self) -> Option<List> {
        read(&self.last_path()).map(|(_, list)| list).filter(|list| !list.entries.is_empty())
    }

    /// Replaces the last tabs. A failure is not said: nothing the user asked for depends on it.
    pub(super) fn keep_last(&self, list: &List) {
        let _ = self.write(None, list).and_then(|temp| temp.replace(&self.last_path()));
    }

    /// The named sets, in the order of their names.
    pub(super) fn named(&self) -> Vec<Named> {
        let mut sets: Vec<Named> = fs::read_dir(self.dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let (name, list) = read(&entry.path())?;
                let name = name?;
                // A file moved in under another set's name is not that set.
                (entry.path() == self.set_path(&name)).then_some(Named { name, list })
            })
            .collect();
        sets.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name)));
        sets
    }

    /// Saves `list` under `name`. A name that has a set already is refused unless told to replace it.
    pub(super) fn save(&self, name: &str, list: &List, collision: Collision) -> Result<(), Refusal> {
        const FAILED: Refusal = Refusal::Said("The set could not be saved.");
        let name = clean_name(name)?;
        if list.entries.is_empty() {
            return Err(Refusal::Said("There are no tabs to save."));
        }
        let temp = self.write(Some(name), list).map_err(|_| FAILED)?;
        let path = self.set_path(name);
        match collision {
            Collision::Replace => temp.replace(&path).map_err(|_| FAILED),
            // A link is refused where the name exists, so two saves cannot both be the first.
            Collision::Refuse => match fs::hard_link(temp.path(), &path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(Refusal::Taken),
                Err(_) => Err(FAILED),
            },
        }
    }

    /// Gives the set called `from` the name `to`, keeping its list. Never replaces another set.
    pub(super) fn rename(&self, from: &str, to: &str) -> Result<(), Refusal> {
        const FAILED: Refusal = Refusal::Said("The set could not be renamed.");
        let to = clean_name(to)?;
        if to == from {
            return Ok(());
        }
        let (_, list) = read(&self.set_path(from)).ok_or(FAILED)?;
        self.save(to, &list, Collision::Refuse)?;
        fs::remove_file(self.set_path(from)).map_err(|_| FAILED)
    }

    /// Removes the saved list called `name`, and nothing else.
    pub(super) fn delete(&self, name: &str) -> bool {
        fs::remove_file(self.set_path(name)).is_ok()
    }

    /// Writes a list beside where it will be kept, readable only by the user.
    fn write(&self, name: Option<&str>, list: &List) -> io::Result<Temp> {
        let dir = private_dir(&self.state, &self.folder)?;
        let staged = dir.join(format!(".new-{}", std::process::id()));
        // A process of the same number may have died while writing one.
        let _ = fs::remove_file(&staged);
        let (temp, mut file) = Temp::create(staged)?;
        file.write_all(text(name, list).as_bytes())?;
        file.sync_all()?;
        Ok(temp)
    }
}

/// A list as it is written: a line for each fact, and a tab's path last on its line.
fn text(name: Option<&str>, list: &List) -> String {
    let mut out = format!("{HEADER}\n");
    if let Some(name) = name {
        out.push_str(&format!("name {name}\n"));
    }
    let kept = &list.entries[..list.entries.len().min(MOST_TABS)];
    out.push_str(&format!("active {}\n", list.active.min(kept.len().saturating_sub(1))));
    for entry in kept {
        let path = entry.path.replace('\\', "\\\\").replace('\n', "\\n");
        match &entry.commit {
            Some(id) => out.push_str(&format!("commit {id} {} {path}\n", entry.line)),
            None => out.push_str(&format!("working {} {path}\n", entry.line)),
        }
    }
    out
}

/// The name and list in the file at `path`, if it is a list this version wrote.
fn read(path: &Path) -> Option<(Option<String>, List)> {
    let file: File = fs_ops::open_read(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(LIMIT + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > LIMIT {
        return None;
    }
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return None;
    }
    let (mut name, mut list) = (None, List::default());
    for line in lines {
        let (word, rest) = line.split_once(' ')?;
        match word {
            "name" => name = Some(clean_name(rest).ok()?.to_owned()),
            "active" => list.active = rest.parse().ok()?,
            "working" => {
                let (line, path) = rest.split_once(' ')?;
                list.entries.push(Entry { path: unescaped(path)?, commit: None, line: line.parse().ok()? });
            }
            "commit" => {
                let mut fields = rest.splitn(3, ' ');
                let (id, line, path) = (fields.next()?, fields.next()?, fields.next()?);
                // Only a whole ID names one commit for good.
                let whole = matches!(id.len(), 40 | 64) && CommitId::parse(id).is_some();
                let commit = whole.then(|| id.to_owned())?;
                list.entries.push(Entry { path: unescaped(path)?, commit: Some(commit), line: line.parse().ok()? });
            }
            _ => return None,
        }
    }
    (list.entries.len() <= MOST_TABS).then_some(())?;
    list.active = list.active.min(list.entries.len().saturating_sub(1));
    Some((name, list))
}

/// A path as it was before it was written, if it stays inside the worktree.
fn unescaped(written: &str) -> Option<String> {
    let mut path = String::with_capacity(written.len());
    let mut chars = written.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => path.push(match chars.next()? {
                'n' => '\n',
                '\\' => '\\',
                _ => return None,
            }),
            c => path.push(c),
        }
    }
    let inside = !path.is_empty() && Path::new(&path).components().all(|part| matches!(part, Component::Normal(_)));
    inside.then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    const ID: &str = "ca3065e5729199ac4562442eea80513faff45a76";

    fn list(paths: &[&str]) -> List {
        let entries = paths.iter().map(|path| Entry { path: (*path).into(), commit: None, line: 3 }).collect();
        List { entries, active: 0 }
    }

    #[test]
    fn a_list_comes_back_as_it_was_saved_and_is_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let f = Fixture::folder();
        let shelf = Shelf::new(f.root.join("state"), &f.root);
        assert!(shelf.last().is_none() && shelf.named().is_empty());
        let mut kept = list(&["src/a.rs", "odd \\ name\nwith a break.txt"]);
        kept.entries.push(Entry { path: "src/a.rs".into(), commit: Some(ID.into()), line: 41 });
        kept.active = 2;
        shelf.keep_last(&kept);
        assert_eq!(shelf.last().as_ref(), Some(&kept));
        assert_eq!(shelf.save("  Rex compatibility ", &kept, Collision::Refuse), Ok(()));
        assert_eq!(shelf.named(), [Named { name: "Rex compatibility".into(), list: kept }]);
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&shelf.dir()), mode(&shelf.last_path())), (0o700, 0o600));
        // Nothing of a list is written into the project.
        assert_eq!(fs::read_dir(&f.root).unwrap().count(), 1);
    }

    #[test]
    fn a_taken_name_is_refused_until_replacing_is_asked_for() {
        let f = Fixture::folder();
        let shelf = Shelf::new(f.root.join("state"), &f.root);
        let (first, second) = (list(&["a.txt"]), list(&["b.txt", "c.txt"]));
        assert_eq!(shelf.save("Parser", &first, Collision::Refuse), Ok(()));
        assert_eq!(shelf.save("Parser", &second, Collision::Refuse), Err(Refusal::Taken));
        assert_eq!(shelf.named()[0].list, first);
        assert_eq!(shelf.save("Parser", &second, Collision::Replace), Ok(()));
        assert_eq!(shelf.named()[0].list, second);

        // A rename keeps the list and never lands on another set.
        assert_eq!(shelf.save("Other", &first, Collision::Refuse), Ok(()));
        assert_eq!(shelf.rename("Parser", "Other"), Err(Refusal::Taken));
        assert_eq!(shelf.rename("Parser", "Lexer"), Ok(()));
        let names: Vec<_> = shelf.named().into_iter().map(|set| (set.name, set.list.entries.len())).collect();
        assert_eq!(names, [("Lexer".to_owned(), 2), ("Other".to_owned(), 1)]);
        assert!(shelf.delete("Lexer") && !shelf.delete("Lexer"));
        assert_eq!(shelf.named().len(), 1);
        assert!(matches!(shelf.save(" \t", &first, Collision::Refuse), Err(Refusal::Said(_))));
        assert!(matches!(shelf.save("Empty", &List::default(), Collision::Refuse), Err(Refusal::Said(_))));
    }

    #[test]
    fn each_worktree_has_its_own_lists_and_a_damaged_one_is_no_list() {
        let f = Fixture::folder();
        let state = f.root.join("state");
        let (here, there) = (Shelf::new(state.clone(), &f.root.join("one")), Shelf::new(state, &f.root.join("two")));
        assert_eq!(here.save("Same name", &list(&["a.txt"]), Collision::Refuse), Ok(()));
        assert_eq!(there.save("Same name", &list(&["b.txt"]), Collision::Refuse), Ok(()));
        assert_eq!(here.named()[0].list.entries[0].path, "a.txt");
        assert_eq!(there.named()[0].list.entries[0].path, "b.txt");

        // A path that would leave the worktree, a short commit ID and an unknown line each void the file.
        for body in ["working 0 ../outside.txt", "working 0 /etc/passwd", "commit ca3065e 0 a.txt", "later 1"] {
            fs::write(here.last_path(), format!("{HEADER}\nactive 0\n{body}\n")).unwrap();
            assert!(here.last().is_none(), "{body}");
        }
        fs::write(here.last_path(), format!("{HEADER}\nactive 9\nworking 2 a.txt\n")).unwrap();
        assert_eq!(here.last().unwrap().active, 0);
    }
}
