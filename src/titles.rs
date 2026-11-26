//! Names Rex tabs and sessions after a shell's Git project and remembers the tab titles Milo set.
//!
//! A title is made from folder names and the name of an agent's process.
//! Nothing here reads a terminal's screen, its title or what was typed.
//! A *claim* records the title Milo last gave a tab, by Rex window ID, so
//! that a restarted listener can tell its own titles from names people chose.
//! An answer's *grounds* are the files it depends on. While they read the
//! same, the listener reuses the answer instead of starting Milo and Git again.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use crate::git::{self, Checkout};
use crate::preferences::{self, Preferences};
use crate::{paths, rex};

/// Rex refuses a longer label.
const LABEL_LIMIT: usize = 128;
const CLAIMS_FILE: &str = "tab-titles";
const CLAIMS_LIMIT: u64 = 1 << 20;
/// Rex's default limits allow this many tabs on one server.
const CLAIMS_KEPT: usize = 4096;
const BAD_ARGS: &str = "Invalid tab-title arguments.";
const NO_PROJECT: &str = "This shell is not in a Git project.";
const BAD_STATE: &str = "Could not update Milo's remembered tab titles.";

/// Internal, noninteractive entry point for the installed Rex listener and actions.
pub fn command(args: &[OsString]) -> i32 {
    let reply = match (paths::config_dir(), paths::state_dir()) {
        (Some(config), Some(state)) => run(args, &config, &state),
        _ => Err("Could not find Milo's settings folders."),
    };
    match reply {
        Ok(text) => {
            print!("{text}");
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn run(args: &[OsString], config: &Path, state: &Path) -> Result<String, &'static str> {
    let claims = Claims(state.join(CLAIMS_FILE));
    let window = |id: &OsString| rex::exact_id(id, "window:").map(str::to_owned).ok_or(BAD_ARGS);
    let [action, rest @ ..] = args else { return Err(BAD_ARGS) };
    match (action.to_str(), rest) {
        // The explicit session action names any Git project, whatever the tab-title choice.
        (Some("name"), [cwd]) => {
            let checkout = git::checkout(Path::new(cwd))?.ok_or(NO_PROJECT)?;
            let label = label(&checkout, None);
            if label.is_empty() {
                return Err(NO_PROJECT);
            }
            Ok(format!("{label}\n"))
        }
        // An empty title leaves the tab with Rex's own label.
        (Some("title"), [cwd, agent @ ..]) if agent.len() <= 1 => {
            let agent = agent.first().map(|name| agent_name(name).ok_or(BAD_ARGS)).transpose()?;
            let cwd = Path::new(cwd);
            let (title, grounds) = match git::checkout(cwd) {
                Ok(Some(checkout)) => {
                    let (prefs, _) = Preferences::for_pane_in(config, Some(&checkout.root));
                    let title = if prefs.tab_titles { label(&checkout, agent) } else { String::new() };
                    (title, preferences::layers(config, &checkout.root).to_vec())
                }
                Ok(None) => (String::new(), outside_grounds(cwd)),
                // Git could not be asked, which may not be so next time.
                Err(_) => (String::new(), Vec::new()),
            };
            // A path through a link can come to mean another folder without any file changing.
            let settled = crate::sys::canonicalize(cwd).is_ok_and(|real| real == cwd);
            Ok(answer(&title, if settled { &grounds } else { &[] }))
        }
        (Some("claim"), [id, title]) => {
            let title = title.to_str().filter(|title| storable(title));
            claims.set(&window(id)?, title.ok_or(BAD_ARGS)?).map_err(|_| BAD_STATE)?;
            Ok(String::new())
        }
        (Some("owned"), []) => Ok(encode(&claims.load().map_err(|_| BAD_STATE)?)),
        (Some("forget"), [_, ..]) => {
            let ids = rest.iter().map(window).collect::<Result<Vec<_>, _>>()?;
            claims.forget(&ids).map_err(|_| BAD_STATE)?;
            Ok(String::new())
        }
        _ => Err(BAD_ARGS),
    }
}

/// A title, then its grounds on a line each. Grounds that a line cannot carry
/// are all left out, and the listener asks again about an answer without any.
fn answer(title: &str, grounds: &[PathBuf]) -> String {
    let mut text = format!("={title}\n");
    let lines: Option<Vec<&str>> = grounds
        .iter()
        .map(|path| path.to_str().filter(|path| path.starts_with('/') && !path.contains(['\n', '\r'])))
        .collect();
    for line in lines.unwrap_or_default() {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// Outside a project, an answer holds until a repository appears at `dir` or
/// above it, so its grounds are the places where one could. There are none if
/// something is already in such a place, since Git has just refused it and
/// may not next time. There are none either if another user could put a file
/// there: the listener opens what it finds, and must not be handed their pipe.
fn outside_grounds(dir: &Path) -> Vec<PathBuf> {
    let mut grounds = Vec::new();
    for folder in dir.ancestors() {
        let git = folder.join(".git");
        if git.symlink_metadata().is_ok() || !private(folder) {
            return Vec::new();
        }
        // A linked worktree's `.git` is a file. A repository's is a folder, and
        // whether a folder opens as a file is up to the system, so name a file in it too.
        grounds.push(git.join("HEAD"));
        grounds.push(git);
    }
    grounds
}

/// Whether only this user and the system can add files to `folder`.
#[cfg(unix)]
fn private(folder: &Path) -> bool {
    // SAFETY: `geteuid` takes no arguments and only reads this process's user ID.
    let user = unsafe { libc::geteuid() };
    folder.metadata().is_ok_and(|meta| meta.mode() & 0o022 == 0 && (meta.uid() == user || meta.uid() == 0))
}

/// Windows has no owner and mode to check here, so no folder counts as private.
#[cfg(windows)]
fn private(_folder: &Path) -> bool {
    false
}

/// The listener passes the name of a process it recognised, never free text.
fn agent_name(name: &OsStr) -> Option<&str> {
    let name = name.to_str()?;
    let plain = name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    ((1..=16).contains(&name.len()) && plain).then_some(name)
}

/// The name for a session made to work in `dir`: `project`, or
/// `project/worktree` in a linked worktree. None outside a Git project.
pub(crate) fn session_name(dir: &Path) -> Option<String> {
    let name = label(&git::checkout(dir).ok()??, None);
    (!name.is_empty()).then_some(name)
}

/// `project`, or `project/worktree` in a linked worktree, then the agent running there.
fn label(checkout: &Checkout, agent: Option<&str>) -> String {
    let mut name = checkout.project.clone();
    if let Some(worktree) = &checkout.worktree {
        name.push('/');
        name.push_str(worktree);
    }
    let agent = agent.map(|agent| format!(" · {agent}")).unwrap_or_default();
    let mut label = fit(&name, LABEL_LIMIT - agent.len());
    if !label.is_empty() {
        label.push_str(&agent);
    }
    label
}

/// Rex trims a label and refuses controls. Do both here, within a budget of
/// UTF-8 bytes, so the label Rex stores is exactly the one Milo remembers.
fn fit(text: &str, limit: usize) -> String {
    let mut left = limit;
    let mut kept: String = text
        .chars()
        .filter(|c| !c.is_control())
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| left.checked_sub(c.len_utf8()).map(|rest| left = rest).is_some())
        .collect();
    kept.truncate(kept.trim_end().len());
    kept
}

fn storable(title: &str) -> bool {
    !title.is_empty() && title.len() <= LABEL_LIMIT && title.trim() == title && !title.chars().any(char::is_control)
}

/// The title Milo last gave one tab.
struct Claim {
    window: String,
    title: String,
}

fn encode(claims: &[Claim]) -> String {
    let mut text = String::new();
    for claim in claims {
        writeln!(text, "{}\t{}", claim.window, claim.title).expect("writing to a string");
    }
    text
}

struct Claims(PathBuf);

impl Claims {
    /// Oldest first. A line that is not a claim is skipped.
    fn load(&self) -> io::Result<Vec<Claim>> {
        let text = preferences::read(&self.0, CLAIMS_LIMIT)?.unwrap_or_default();
        Ok(text
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .filter(|(window, title)| rex::exact_id(OsStr::new(window), "window:").is_some() && storable(title))
            .map(|(window, title)| Claim { window: window.to_owned(), title: title.to_owned() })
            .collect())
    }

    fn set(&self, window: &str, title: &str) -> io::Result<()> {
        self.change(|claims| {
            if let Some(i) = claims.iter().position(|claim| claim.window == window) {
                if claims[i].title == title {
                    return false;
                }
                // A changed title is newly claimed, even if the window is old.
                let mut claim = claims.remove(i);
                title.clone_into(&mut claim.title);
                claims.push(claim);
            } else {
                claims.push(Claim { window: window.to_owned(), title: title.to_owned() });
            }
            true
        })
    }

    fn forget(&self, windows: &[String]) -> io::Result<()> {
        // Do not create the state folder to find that it holds no claims.
        if !self.0.exists() {
            return Ok(());
        }
        self.change(|claims| {
            let before = claims.len();
            claims.retain(|claim| !windows.contains(&claim.window));
            claims.len() != before
        })
    }

    /// `edit` says whether it changed anything. Only the newest claims are kept.
    fn change(&self, edit: impl FnOnce(&mut Vec<Claim>) -> bool) -> io::Result<()> {
        preferences::locked(&self.0, |dir| {
            let mut claims = self.load()?;
            if !edit(&mut claims) {
                return Ok(());
            }
            let kept = &claims[claims.len().saturating_sub(CLAIMS_KEPT)..];
            preferences::replace(dir, &self.0, &encode(kept))
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::fixture::Fixture;
    use crate::preferences::PreferenceSnapshot;

    const TAB: &str = "window:01a1f929-dc65-7cc5-9ba2-cac3108697bf";
    const OTHER: &str = "window:01a1f929-b154-761a-bf02-412983fd1b7d";

    struct Homes {
        fixture: Fixture,
    }

    impl Homes {
        fn new() -> Self {
            Self { fixture: Fixture::folder() }
        }

        fn run(&self, args: &[&str]) -> Result<String, &'static str> {
            let args: Vec<OsString> = args.iter().map(OsString::from).collect();
            run(&args, &self.fixture.root.join("config"), &self.fixture.root.join("state"))
        }

        /// The title Milo answers with and the grounds it gives for it.
        fn asked(&self, about: &[&str]) -> (String, Vec<PathBuf>) {
            let reply = self.run(&[&["title"], about].concat()).unwrap();
            let mut lines = reply.lines();
            let title = lines.next().and_then(|line| line.strip_prefix('=')).expect("a title line").to_owned();
            (title, lines.map(PathBuf::from).collect())
        }

        fn title(&self, about: &[&str]) -> String {
            self.asked(about).0
        }

        /// Save the choice the way Settings does, globally or for one checkout.
        fn choose(&self, worktree: Option<&Path>, on: bool) {
            let config = self.fixture.root.join("config");
            let saved = PreferenceSnapshot::load(&config, worktree).unwrap();
            saved.save(Preferences { tab_titles: on, ..saved.prefs }, &config, worktree, worktree.is_some()).unwrap();
        }

        fn claims(&self) -> String {
            std::fs::read_to_string(self.fixture.root.join("state").join(CLAIMS_FILE)).unwrap_or_default()
        }
    }

    fn project() -> Fixture {
        let project = Fixture::new();
        project.write("page.txt", "a file\n");
        project.commit("first");
        project
    }

    #[test]
    fn titles_are_off_until_chosen_and_then_name_the_project_worktree_and_agent() {
        let homes = Homes::new();
        let project = project();
        project.git(&["worktree", "add", "-q", "../feature"]);
        let main = project.root.join("subfolder");
        std::fs::create_dir(&main).unwrap();
        let main = main.to_str().unwrap();
        let linked = project.root.with_file_name("feature");
        let linked = linked.to_str().unwrap();
        assert_eq!(homes.title(&[main, "claude"]), "");

        homes.choose(None, true);
        assert_eq!(homes.title(&[main]), "project");
        assert_eq!(homes.title(&[main, "claude"]), "project · claude");
        assert_eq!(homes.title(&[linked, "omp"]), "project/feature · omp");
        let plain = Fixture::folder();
        assert_eq!(homes.title(&[plain.root.to_str().unwrap(), "claude"]), "");
        assert_eq!(homes.claims(), "", "asking for a title remembers nothing");
    }

    #[test]
    fn a_worktree_can_choose_titles_apart_from_the_global_default() {
        let homes = Homes::new();
        let on = project();
        let off = project();
        homes.choose(Some(&on.root), true);
        assert_eq!(homes.title(&[on.root.to_str().unwrap()]), "project");
        assert_eq!(homes.title(&[off.root.to_str().unwrap()]), "");

        homes.choose(None, true);
        homes.choose(Some(&off.root), false);
        assert_eq!(homes.title(&[off.root.to_str().unwrap()]), "");
    }

    #[test]
    fn a_project_answer_stands_on_the_preferences_that_can_change_it() {
        let homes = Homes::new();
        let project = project();
        let inside = project.root.join("deep");
        std::fs::create_dir(&inside).unwrap();
        let inside = inside.to_str().unwrap();
        let read = |grounds: &[PathBuf]| grounds.iter().map(|file| std::fs::read(file).ok()).collect::<Vec<_>>();

        // The grounds are named before either file exists, so that a first choice is noticed.
        let (title, grounds) = homes.asked(&[inside, "codex"]);
        let unset = read(&grounds);
        assert!(title.is_empty() && !grounds.is_empty() && unset.iter().all(Option::is_none));
        homes.choose(Some(&project.root), true);
        let here = read(&grounds);
        assert_ne!(here, unset);
        homes.choose(None, true);
        assert_ne!(read(&grounds), here);
        assert_eq!(homes.asked(&[inside]), ("project".to_owned(), grounds));
    }

    #[test]
    fn an_answer_outside_a_project_stands_until_a_repository_appears() {
        let homes = Homes::new();
        homes.choose(None, true);
        let plain = Fixture::folder();
        let deep = plain.root.join("a/b");
        std::fs::create_dir_all(&deep).unwrap();
        let (title, grounds) = homes.asked(&[deep.to_str().unwrap()]);
        assert!(title.is_empty() && grounds.iter().all(|file| std::fs::read(file).is_err()));
        // Where others can write above the temporary folder, as in `/tmp`, nothing can stand.
        let private_above = plain.root.ancestors().all(private);
        let named = |spot: PathBuf| grounds.contains(&spot);
        assert_eq!(named(deep.join(".git")) && named(plain.root.join(".git/HEAD")), private_above);
        plain.git(&["init", "-q", "--template=", "-b", "main"]);
        assert_eq!(grounds.iter().any(|file| std::fs::read(file).is_ok()), private_above);
        assert_eq!(homes.title(&[deep.to_str().unwrap()]), "project");
    }

    #[cfg(unix)]
    #[test]
    fn an_answer_that_may_not_last_has_no_grounds() {
        let homes = Homes::new();
        homes.choose(None, true);
        let nothing = (String::new(), Vec::new());
        // A repository Git will not use might be put right.
        let refused = Fixture::folder();
        std::fs::create_dir(refused.root.join(".git")).unwrap();
        assert_eq!(homes.asked(&[refused.root.to_str().unwrap()]), nothing);

        // Where anyone may add files, the listener is not sent to open what turns up.
        let open = Fixture::folder();
        let inside = open.root.join("shared/inside");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::set_permissions(open.root.join("shared"), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(homes.asked(&[inside.to_str().unwrap()]), nothing);

        // A link can be pointed at another project, which no preference file would show.
        let project = project();
        let link = open.root.join("link");
        std::os::unix::fs::symlink(&project.root, &link).unwrap();
        assert_eq!(homes.asked(&[link.to_str().unwrap()]), ("project".to_owned(), Vec::new()));
    }

    #[test]
    fn grounds_that_a_line_cannot_carry_are_all_left_out() {
        assert_eq!(answer("project", &[PathBuf::from("/my config/milo")]), "=project\n/my config/milo\n");
        assert_eq!(answer("project", &[PathBuf::from("/config"), PathBuf::from("/a\nb")]), "=project\n");
        assert_eq!(answer("", &[PathBuf::from("relative")]), "=\n");
    }

    #[test]
    fn a_session_name_needs_a_git_project_but_not_the_title_preference() {
        let homes = Homes::new();
        let project = project();
        project.git(&["worktree", "add", "-q", "../feature"]);
        let linked = project.root.with_file_name("feature");
        let inside = linked.join("deep");
        std::fs::create_dir(&inside).unwrap();
        assert_eq!(homes.run(&["name", inside.to_str().unwrap()]).unwrap(), "project/feature\n");
        let plain = Fixture::folder();
        assert_eq!(homes.run(&["name", plain.root.to_str().unwrap()]), Err(NO_PROJECT));
    }

    #[test]
    fn labels_are_exactly_what_rex_will_store() {
        let named = |project: &str, worktree: Option<&str>, agent| {
            let checkout =
                Checkout { root: PathBuf::new(), project: project.into(), worktree: worktree.map(Into::into) };
            label(&checkout, agent)
        };
        assert_eq!(named("  my\tapp\u{85} ", None, None), "myapp");
        assert_eq!(named(&format!("\u{7f}{}myapp", " ".repeat(LABEL_LIMIT)), None, None), "myapp");
        assert_eq!(named("\u{1}\n", None, Some("codex")), "", "an unnameable project gets no title at all");
        let long = "é".repeat(100);
        let title = named(&long, Some("tree"), Some("claude"));
        assert_eq!(title, format!("{} · claude", "é".repeat(59)));
        assert!(title.len() <= LABEL_LIMIT);
        assert_eq!(named(&format!("{} x", "a".repeat(127)), None, None), "a".repeat(127));
    }

    #[test]
    fn claims_are_kept_by_window_until_forgotten() {
        let homes = Homes::new();
        assert_eq!(homes.run(&["owned"]).unwrap(), "");
        homes.run(&["forget", TAB]).unwrap();
        assert!(!homes.fixture.root.join("state").exists(), "forgetting nothing writes nothing");

        homes.run(&["claim", TAB, "project"]).unwrap();
        homes.run(&["claim", OTHER, "project/feature · omp"]).unwrap();
        homes.run(&["claim", TAB, "project · claude"]).unwrap();
        assert_eq!(
            homes.run(&["owned"]).unwrap(),
            format!("{OTHER}\tproject/feature · omp\n{TAB}\tproject · claude\n"),
        );
        homes.run(&["forget", TAB]).unwrap();
        assert_eq!(homes.claims(), format!("{OTHER}\tproject/feature · omp\n"));
        homes.run(&["forget", TAB, OTHER]).unwrap();
        assert_eq!(homes.claims(), "");
    }

    #[test]
    fn only_whole_window_ids_plain_agents_and_storable_titles_are_accepted() {
        let homes = Homes::new();
        let project = project();
        let cwd = project.root.to_str().unwrap();
        homes.choose(None, true);
        let long = "a".repeat(LABEL_LIMIT + 1);
        for args in [
            &["title", cwd, "Claude Code"][..],
            &["title", cwd, "a\tb"],
            &["title", cwd, "claude", "codex"],
            &["claim", "window:short", "project"],
            &["claim", "2", "project"],
            &["claim", TAB, ""],
            &["claim", TAB, " project"],
            &["claim", TAB, "pro\tject"],
            &["claim", TAB, &long],
            &["forget"],
            &["forget", TAB, "main"],
            &["owned", TAB],
        ] {
            assert_eq!(homes.run(args), Err(BAD_ARGS), "{args:?}");
        }
        assert_eq!(homes.claims(), "");
    }

    #[test]
    fn remembered_titles_survive_damage_and_keep_the_newest_tabs() {
        let homes = Homes::new();
        let id = |n: usize| format!("window:00000000-0000-0000-0000-{n:012x}");
        let mut text = format!("not a claim\nwindow:short\tname\n{TAB}\tbad\tclaim\n");
        for n in 0..CLAIMS_KEPT {
            writeln!(text, "{}\tolder", id(n)).unwrap();
        }
        homes.fixture.write(&format!("state/{CLAIMS_FILE}"), &text);
        assert!(!homes.run(&["owned"]).unwrap().contains(TAB));
        homes.run(&["claim", &id(0), "changed"]).unwrap();
        homes.run(&["claim", TAB, "project"]).unwrap();
        let kept = homes.claims();
        assert_eq!(kept.lines().count(), CLAIMS_KEPT);
        assert!(kept.starts_with(&format!("{}\tolder\n", id(2))));
        assert!(kept.ends_with(&format!("{}\tchanged\n{TAB}\tproject\n", id(0))));
    }
}
