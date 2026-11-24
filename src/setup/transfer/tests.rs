//! Checks bundles, project matching, conflicts and undo with throwaway settings folders.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::fixture::Fixture;

/// One host: settings folders of its own, and optionally a checkout it is working in.
struct Host {
    folder: Fixture,
    worktree: Option<PathBuf>,
}

impl Host {
    fn new() -> Self {
        Self { folder: Fixture::folder(), worktree: None }
    }

    fn config(&self) -> PathBuf {
        self.folder.root.join("config")
    }

    fn state(&self) -> PathBuf {
        self.folder.root.join("state")
    }

    fn with<T>(&self, run: impl FnOnce(&Place<'_>) -> T) -> T {
        let (config, state) = (self.config(), self.state());
        run(&Place {
            config: &config,
            state: &state,
            home: Some(&self.folder.root),
            worktree: self.worktree.as_deref(),
        })
    }

    fn write(&self, file: &str, text: &str) {
        let path = self.config().join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn read(&self, file: &str) -> Option<String> {
        fs::read_to_string(self.config().join(file)).ok()
    }

    /// Every file below the settings folder with its bytes, to compare a host with itself later.
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            let Ok(entries) = fs::read_dir(dir) else { return };
            for entry in entries.map(Result::unwrap) {
                let path = entry.path();
                if path.is_dir() { walk(&path, out) } else { out.push((path.clone(), fs::read(&path).unwrap())) }
            }
        }
        let mut files = Vec::new();
        walk(&self.config(), &mut files);
        files.sort();
        files
    }

    fn worktree_layer(&self, root: &Path) -> String {
        fs::read_to_string(preferences::layer_file(&self.config(), Some(root))).unwrap_or_default()
    }
}

const RAIL: &str = "v1 r 0.30 0.70 2 ohu 110";

/// A repository with one commit, and a second checkout of it in another folder.
fn two_checkouts() -> (Fixture, Fixture) {
    let first = Fixture::new();
    first.write("README", "hello\n");
    first.commit("start");
    let second = Fixture::new();
    second.git(&["pull", "-q", first.root.to_str().unwrap(), "main"]);
    (first, second)
}

fn seeded() -> Host {
    let host = Host::new();
    host.write(
        "preferences.conf",
        "icons = plain\ntext_git_marks = true\nchanges_only = true\nusage_codex = hide\n\
         usage_deepseek = always\ntab_titles = true\n",
    );
    host.write("layouts/global", &format!("milo-layout-v1 global\n{RAIL}\n"));
    host
}

#[test]
fn an_export_holds_only_the_chosen_groups_and_never_an_account_or_integration_choice() {
    let host = seeded();
    // Things that sit beside the preferences and must stay on the host.
    host.write("usage-accounts.conf", "MILO_OPENROUTER_KEY=sk-seeded-marker\n");
    host.write("alerts.conf", "telegram_chat = -100seeded-marker\n");
    host.write("discord.conf", "enabled = true\n");
    fs::create_dir_all(host.state()).unwrap();
    fs::write(host.state().join("alerts-secrets"), "telegram seeded-marker\n").unwrap();

    let all = host.with(|place| export(place, &Group::ALL, Scopes::Global)).unwrap();
    for kept in ["icons = plain", "text_git_marks = true", "changes_only = true", "usage_codex = hide", RAIL] {
        assert!(all.contains(kept), "{kept} is missing from:\n{all}");
    }
    for left in ["seeded-marker", "deepseek", "tab_titles", "telegram", "discord", "MILO_"] {
        assert!(!all.contains(left), "{left} travelled in:\n{all}");
    }
    assert!(!all.contains(host.folder.root.to_str().unwrap()), "a path of this host travelled in:\n{all}");

    let some = host.with(|place| export(place, &[Group::Appearance], Scopes::Global)).unwrap();
    assert!(some.contains("icons = plain") && !some.contains("changes_only") && !some.contains("layout ="), "{some}");
    assert!(host.with(|place| export(place, &[], Scopes::Global)).is_err());
}

#[test]
fn an_import_gives_another_host_the_chosen_settings_and_leaves_the_first_host_as_it_was() {
    let a = seeded();
    let b = Host::new();
    b.write("preferences.conf", "# kept by hand\nicons = off\nusage_openrouter = always\nfollow_activity = false\n");
    let before_a = a.snapshot();

    let bundle =
        a.with(|place| export(place, &[Group::Appearance, Group::Usage, Group::Layout], Scopes::Global)).unwrap();
    let preview = b.with(|place| plan(place, &bundle, &[])).unwrap();
    let report = preview.report("bundle.txt");
    assert!(report.contains("Icons: off → plain") && report.contains("Codex in Usage: auto → hide"), "{report}");
    assert!(report.contains("Layout: not saved → Outline, History on the right, 30% wide"), "{report}");
    assert_eq!(b.read("preferences.conf").unwrap().lines().nth(1), Some("icons = off"), "a preview writes nothing");

    assert_eq!(b.with(|place| apply(place, preview)).unwrap(), 4);
    let prefs = b.read("preferences.conf").unwrap();
    for line in ["# kept by hand", "icons = plain", "text_git_marks = true", "usage_codex = hide"] {
        assert!(prefs.lines().any(|kept| kept == line), "{line} is missing from:\n{prefs}");
    }
    // What was not chosen, and what never travels, stays as host B had it.
    assert!(prefs.contains("usage_openrouter = always") && prefs.contains("follow_activity = false"), "{prefs}");
    assert!(!prefs.contains("changes_only") && !prefs.contains("deepseek") && !prefs.contains("tab_titles"), "{prefs}");
    assert_eq!(b.read("layouts/global").unwrap(), format!("milo-layout-v1 global\n{RAIL}\n"));
    assert_eq!(a.snapshot(), before_a, "host A was only read");

    // The same bundle a second time has nothing left to do.
    let again = b.with(|place| plan(place, &bundle, &[])).unwrap();
    assert_eq!(again.changes(), 0);
}

#[test]
fn a_bundle_cannot_turn_on_an_account_or_an_integration_or_smuggle_a_path() {
    let b = Host::new();
    let bundle = "milo-settings 1\n[global]\nicons = off\nusage_deepseek = always\ntab_titles = true\n\
                  auto_arrange = true\ntelegram_token = 123:abc\nicons_future = sparkly\nlayout = v9 nonsense\n";
    let preview = b.with(|place| plan(place, bundle, &[])).unwrap();
    assert_eq!(preview.changes(), 1);
    let report = preview.report("f");
    for named in [
        "usage_deepseek (set on each host",
        "tab_titles (set on each host",
        "telegram_token (not a setting",
        "layout (written in a form",
    ] {
        assert!(report.contains(named), "{named} is missing from:\n{report}");
    }
    b.with(|place| apply(place, preview)).unwrap();
    assert_eq!(b.read("preferences.conf").unwrap(), "icons = off\n");
    assert_eq!(b.snapshot().len(), 1, "only the preferences file was written");

    for refused in [
        "",
        "not a bundle",
        "milo-settings 1\nicons = plain\n",
        "milo-settings 1\n[global]\n[global]\n",
        "milo-settings 1\n[global]\nicons = plain\x1b[31m\n",
        "milo-settings 1\n[project]\nname = ../../etc\n",
        "milo-settings 1\n[global]\nno equals sign\n",
    ] {
        assert_eq!(b.with(|place| plan(place, refused, &[])).err().as_deref(), Some(NOT_A_BUNDLE), "{refused:?}");
    }
    let newer = b.with(|place| plan(place, "milo-settings 2\n[global]\nicons = plain\n", &[])).err();
    assert_eq!(newer.as_deref(), Some(NEWER_BUNDLE));
    let large = format!("milo-settings 1\n[global]\n{}", "# padding\n".repeat(BUNDLE_LIMIT / 10 + 1));
    assert_eq!(b.with(|place| plan(place, &large, &[])).err().as_deref(), Some(NOT_A_BUNDLE));
}

#[test]
fn a_project_follows_its_repository_to_another_path_and_never_a_name_alone() {
    let (first, second) = two_checkouts();
    let mut a = Host::new();
    a.worktree = Some(first.root.clone());
    fs::create_dir_all(a.config().join("worktrees")).unwrap();
    fs::write(preferences::layer_file(&a.config(), Some(&first.root)), "changes_only = true\nicons = off\n").unwrap();
    let rail_owner = super::hex(first.root.as_os_str().as_encoded_bytes());
    let (_, change) = profile_edit(&a.config(), Some(&first.root), Layout::parse_rail(RAIL).as_ref()).unwrap();
    files::install(change.as_slice(), || true).unwrap();
    let bundle = a.with(|place| export(place, &Group::ALL, Scopes::Worktree)).unwrap();
    assert!(bundle.contains("[project]\nname = project\n") && bundle.contains("repository = "), "{bundle}");
    assert!(!bundle.contains(first.root.to_str().unwrap()) && !bundle.contains(&rail_owner), "{bundle}");
    assert!(
        !bundle.contains("[global]") && !bundle.contains("follow_activity"),
        "only the overrides travel:\n{bundle}"
    );

    // Host B has the same repository checked out somewhere else.
    let mut b = Host::new();
    b.worktree = Some(second.root.clone());
    let plan_b = b.with(|place| plan(place, &bundle, &[])).unwrap();
    let Destination::Worktree { root, matched } = &plan_b.sections[0].destination else {
        panic!("{}", plan_b.report("f"))
    };
    assert_eq!((root, matched.reason()), (&second.root, "same repository"));
    assert_eq!(b.with(|place| apply(place, plan_b)).unwrap(), 3);
    assert_eq!(b.worktree_layer(&second.root), "icons = off\nchanges_only = true\n");
    assert!(b.read("preferences.conf").is_none(), "the global defaults were not touched");
    let (saved, _) = profile_edit(&b.config(), Some(&second.root), None).unwrap();
    assert_eq!(saved.map(|layout| layout.rail_code()).as_deref(), Some(RAIL));

    // Host C has an unrelated repository in a folder of the same name.
    let unrelated = Fixture::new();
    unrelated.write("README", "another project\n");
    unrelated.commit("another start");
    let mut c = Host::new();
    c.worktree = Some(unrelated.root.clone());
    let plan_c = c.with(|place| plan(place, &bundle, &[])).unwrap();
    assert_eq!(plan_c.sections[0].destination, Destination::Unmapped);
    assert_eq!(plan_c.changes(), 0);
    assert!(plan_c.report("f.txt").contains("--map project=DIR"), "{}", plan_c.report("f.txt"));
    assert_eq!(c.with(|place| apply(place, plan_c)).unwrap(), 0);
    assert!(c.snapshot().is_empty());

    // Only the person can say that it is the same project after all.
    let maps = [("project".to_owned(), unrelated.root.join("."))];
    let chosen = c.with(|place| plan(place, &bundle, &maps)).unwrap();
    let Destination::Worktree { root, matched } = &chosen.sections[0].destination else {
        panic!("{}", chosen.report("f"))
    };
    assert_eq!((root, matched.reason()), (&unrelated.root, "chosen by you"));
    let missing = [("other".to_owned(), unrelated.root.clone())];
    assert!(c.with(|place| plan(place, &bundle, &missing)).is_err());
    let outside = [("project".to_owned(), c.folder.root.clone())];
    assert!(c.with(|place| plan(place, &bundle, &outside)).is_err(), "a folder outside Git is no worktree");
}

#[test]
fn an_import_stops_when_the_host_changed_after_its_preview() {
    let a = seeded();
    let b = Host::new();
    b.write("preferences.conf", "icons = off\n");
    let bundle = a.with(|place| export(place, &Group::ALL, Scopes::Global)).unwrap();
    let reviewed = b.with(|place| plan(place, &bundle, &[])).unwrap();
    let basis = reviewed.basis().unwrap();
    // Another editor saves on host B before the import is applied.
    b.write("preferences.conf", "icons = nerd\nstatic_marks = true\n");
    let later = b.with(|place| plan(place, &bundle, &[])).unwrap();
    assert_ne!(later.basis().unwrap(), basis, "a new preview is a new plan");
    assert_eq!(b.with(|place| apply(place, reviewed)).err().as_deref(), Some(STALE));
    assert_eq!(b.read("preferences.conf").unwrap(), "icons = nerd\nstatic_marks = true\n");
    assert!(b.read("layouts/global").is_none(), "none of the import was kept");
    assert!(!undo_available(&b.state()));
}

#[test]
fn undo_puts_back_exactly_what_the_last_import_replaced_unless_it_was_changed_again() {
    let a = seeded();
    let b = Host::new();
    b.write("preferences.conf", "# mine\nicons = off\n");
    let before = b.snapshot();
    let bundle = a.with(|place| export(place, &Group::ALL, Scopes::Global)).unwrap();

    assert_eq!(b.with(undo).err().as_deref(), Some(NOTHING_TO_UNDO));
    let import = |host: &Host| host.with(|place| apply(place, plan(place, &bundle, &[]).unwrap())).unwrap();
    import(&b);
    assert!(undo_available(&b.state()) && b.snapshot() != before);
    assert_eq!(b.with(undo).unwrap(), 2);
    assert_eq!(b.snapshot(), before, "the file that existed is back and the new one is gone");
    assert!(!undo_available(&b.state()));

    // A setting changed after the import is not thrown away by an undo.
    import(&b);
    let imported = b.read("preferences.conf").unwrap();
    b.write("preferences.conf", &format!("{imported}follow_activity = false\n"));
    assert_eq!(b.with(undo).err().as_deref(), Some(CHANGED_SINCE_IMPORT));
    assert!(b.read("preferences.conf").unwrap().ends_with("follow_activity = false\n"));
    assert!(b.read("layouts/global").is_some() && undo_available(&b.state()), "nothing was half undone");
}

#[test]
fn a_record_of_an_import_cannot_point_an_undo_outside_the_settings_folder() {
    let b = Host::new();
    let victim = b.folder.root.join("victim");
    fs::write(&victim, "written").unwrap();
    let record = |path: &Path| {
        format!(
            "{JOURNAL_HEADER}\n{}\t600:{}\t-\n",
            super::hex(path.as_os_str().as_encoded_bytes()),
            super::hex(b"written")
        )
    };
    fs::create_dir_all(b.state()).unwrap();
    for path in [victim.clone(), b.config().join("../victim")] {
        fs::write(b.state().join(JOURNAL), record(&path)).unwrap();
        assert!(b.with(undo).is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "written");
    }
}

#[test]
fn the_command_line_asks_for_groups_by_name_and_maps_by_name() {
    let words = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
    let rest = words(&["layout,appearance", "--scope", "all"]);
    let export = Request::parse(Some(OsStr::new("export")), &rest).unwrap();
    assert_eq!(
        export,
        Request::Export { groups: vec![Group::Appearance, Group::Layout], scopes: Scopes::All, output: None }
    );
    let rest = words(&["bundle.txt", "--map", "milo=/srv/work=tree", "--apply"]);
    let import = Request::parse(Some(OsStr::new("import")), &rest).unwrap();
    let maps = vec![("milo".to_owned(), PathBuf::from("/srv/work=tree"))];
    assert_eq!(import, Request::Import { file: OsStr::new("bundle.txt"), maps, apply: true });
    assert_eq!(Request::parse(Some(OsStr::new("undo")), &[]), Ok(Request::Undo));
    for (action, rest) in [
        ("export", words(&[])),
        ("export", words(&["everything"])),
        ("export", words(&["layout,accounts"])),
        ("export", words(&["all", "--scope", "host"])),
        ("import", words(&[])),
        ("import", words(&["f", "--map", "nodir"])),
        ("import", words(&["f", "--map", "=dir"])),
        ("undo", words(&["now"])),
        ("sync", words(&[])),
    ] {
        assert!(Request::parse(Some(OsStr::new(action)), &rest).is_err(), "{action} {rest:?}");
    }
}
