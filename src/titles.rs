//! Names Rex tabs and sessions after a shell's Git project and remembers the tab titles Milo set.
//!
//! A title is made from folder names and the name of an agent's process.
//! Nothing here reads a terminal's screen, its title or what was typed.
//! A *claim* records the title Milo last gave a tab, by Rex window ID, so
//! that a restarted listener can tell its own titles from names people chose.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io;
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
const NO_PROJECT: &str = "This directory is not in a Git project.";
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
            let checkout = git::checkout(Path::new(cwd)).ok_or(NO_PROJECT)?;
            let label = label(&checkout, None);
            let root = checkout.root.to_str().filter(|root| !label.is_empty() && !root.contains('\n'));
            Ok(format!("{label}\n{}\n", root.ok_or(NO_PROJECT)?))
        }
        // An empty title leaves the tab with Rex's own label.
        (Some("title"), [cwd, agent @ ..]) if agent.len() <= 1 => {
            let agent = agent.first().map(|name| agent_name(name).ok_or(BAD_ARGS)).transpose()?;
            let checkout = git::checkout(Path::new(cwd));
            let (prefs, _) =
                Preferences::for_pane_in(config, checkout.as_ref().map(|checkout| checkout.root.as_path()));
            let title = checkout.filter(|_| prefs.tab_titles).map(|checkout| label(&checkout, agent));
            Ok(format!("={}\n", title.unwrap_or_default()))
        }
        (Some("claim"), [id, title]) => {
            let title = title.to_str().filter(|title| !title.is_empty() && fit(title, LABEL_LIMIT) == *title);
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

/// The listener passes the name of a process it recognised, never free text.
fn agent_name(name: &OsStr) -> Option<&str> {
    let name = name.to_str()?;
    let plain = name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    ((1..=16).contains(&name.len()) && plain).then_some(name)
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
    let kept: String = text
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take_while(|c| left.checked_sub(c.len_utf8()).map(|rest| left = rest).is_some())
        .collect();
    kept.trim().to_owned()
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
            .filter(|(window, title)| rex::exact_id(OsStr::new(window), "window:").is_some() && !title.is_empty())
            .map(|(window, title)| Claim { window: window.to_owned(), title: title.to_owned() })
            .collect())
    }

    fn set(&self, window: &str, title: &str) -> io::Result<()> {
        self.change(|claims| {
            match claims.iter_mut().find(|claim| claim.window == window) {
                Some(claim) if claim.title == title => return false,
                Some(claim) => title.clone_into(&mut claim.title),
                None => claims.push(Claim { window: window.to_owned(), title: title.to_owned() }),
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
        assert_eq!(homes.run(&["title", main, "claude"]).unwrap(), "=\n");

        homes.choose(None, true);
        assert_eq!(homes.run(&["title", main]).unwrap(), "=project\n");
        assert_eq!(homes.run(&["title", main, "claude"]).unwrap(), "=project · claude\n");
        assert_eq!(homes.run(&["title", linked, "omp"]).unwrap(), "=project/feature · omp\n");
        let plain = Fixture::folder();
        assert_eq!(homes.run(&["title", plain.root.to_str().unwrap(), "claude"]).unwrap(), "=\n");
        assert_eq!(homes.claims(), "", "asking for a title remembers nothing");
    }

    #[test]
    fn a_worktree_can_choose_titles_apart_from_the_global_default() {
        let homes = Homes::new();
        let on = project();
        let off = project();
        homes.choose(Some(&on.root), true);
        assert_eq!(homes.run(&["title", on.root.to_str().unwrap()]).unwrap(), "=project\n");
        assert_eq!(homes.run(&["title", off.root.to_str().unwrap()]).unwrap(), "=\n");

        homes.choose(None, true);
        homes.choose(Some(&off.root), false);
        assert_eq!(homes.run(&["title", off.root.to_str().unwrap()]).unwrap(), "=\n");
    }

    #[test]
    fn a_session_name_needs_a_git_project_but_not_the_title_preference() {
        let homes = Homes::new();
        let project = project();
        project.git(&["worktree", "add", "-q", "../feature"]);
        let linked = project.root.with_file_name("feature");
        let inside = linked.join("deep");
        std::fs::create_dir(&inside).unwrap();
        assert_eq!(
            homes.run(&["name", inside.to_str().unwrap()]).unwrap(),
            format!("project/feature\n{}\n", linked.display()),
        );
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
            format!("{TAB}\tproject · claude\n{OTHER}\tproject/feature · omp\n"),
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
        let mut text = String::from("not a claim\nwindow:short\tname\n");
        for n in 0..CLAIMS_KEPT {
            writeln!(text, "{}\tolder", id(n)).unwrap();
        }
        homes.fixture.write(&format!("state/{CLAIMS_FILE}"), &text);
        homes.run(&["claim", TAB, "project"]).unwrap();
        let kept = homes.claims();
        assert_eq!(kept.lines().count(), CLAIMS_KEPT);
        assert!(kept.starts_with(&format!("{}\tolder\n", id(1))) && kept.ends_with(&format!("{TAB}\tproject\n")));
    }
}
