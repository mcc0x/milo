use super::*;
use crate::fixture::Fixture;

fn parsed(words: &[&str]) -> Result<String, String> {
    let rest: Vec<OsString> = words.iter().map(Into::into).collect();
    Request::parse(Some(OsStr::new("new")), &rest).map(|request| format!("{request:?}"))
}

#[test]
fn a_request_names_one_agent_and_one_place_and_keeps_the_prompt_as_written() {
    let prompt = "--agent codex; $(reboot)\n`id`";
    let rest: Vec<OsString> =
        ["--branch", "perf/scroll", "--base", "origin/main", "--agent", "claude", "--prompt", prompt]
            .map(Into::into)
            .into();
    let request = Request::parse(Some(OsStr::new("new")), &rest).unwrap();
    assert_eq!(request.agent, "claude");
    assert_eq!(request.place, Place::Branch { name: "perf/scroll", base: Some("origin/main"), path: None });
    assert_eq!(request.prompt, Some(prompt), "what follows --prompt is the prompt, whatever it looks like");
    assert_eq!((request.project, request.host), (None, None));

    let existing = parsed(&["--agent", "omp", "--worktree", "../other", "--prompt", ""]).unwrap();
    assert!(existing.contains("Worktree(\"../other\")") && existing.contains("prompt: None"), "{existing}");

    for (words, why) in [
        (&["--branch", "b", "--base", "main"][..], "--agent is required: one of claude, codex, omp, opencode"),
        (&["--agent", "cursor", "--branch", "b"], "cursor is not an agent milo can start"),
        (&["--agent", "openrouter", "--branch", "b"], "openrouter is not an agent milo can start"),
        (&["--agent", "claude"], "say where the agent works"),
        (&["--agent", "claude", "--worktree", ".", "--branch", "b"], "not both"),
        (&["--agent", "claude", "--worktree", ".", "--base", "main"], "belong to --branch"),
        (&["--agent", "claude", "--branch", "b", "--branch", "c"], "--branch was given twice"),
        (&["--agent", "claude", "--branch"], "--branch needs a value"),
        (&["--agent", "claude", "--branch", "b", "--host", "-S"], "--host requires a host label"),
        (&["--agent", "claude", "--branch", "b", "--print"], "unexpected argument --print"),
        (&["--agent", "claude", "--branch", "b", "stray"], "unexpected argument stray"),
    ] {
        let refused = parsed(words).unwrap_err();
        assert!(refused.contains(why), "{words:?}: {refused}");
    }
    assert!(Request::parse(None, &[]).unwrap_err().contains("expected milo task new"));
    assert!(Request::parse(Some(OsStr::new("list")), &[]).unwrap_err().contains("expected milo task new"));
}

/// A project with one commit on `main`, a `parked` branch and a linked worktree on `topic`.
fn project() -> (Fixture, PathBuf) {
    let f = Fixture::new();
    f.write("page.txt", "one\n");
    f.commit("first");
    f.git(&["branch", "parked"]);
    let linked = f.root.parent().unwrap().join("linked");
    f.git(&["worktree", "add", "-q", "-b", "topic", linked.to_str().unwrap()]);
    (f, linked)
}

fn branch<'a>(name: &'a str, base: Option<&'a str>, path: Option<&'a OsStr>) -> Place<'a> {
    Place::Branch { name, base, path }
}

#[test]
fn a_plan_names_the_exact_worktree_or_refuses_without_creating_anything() {
    let (f, linked) = project();
    let home = f.root.parent().unwrap().to_owned();

    // An existing worktree is used only when it belongs to this project.
    let Plan::Existing(found) = plan(&f.root, &Place::Worktree(linked.as_os_str())).unwrap() else { panic!() };
    assert_eq!(found, linked);
    let Plan::Existing(own) = plan(&linked, &Place::Worktree(f.root.as_os_str())).unwrap() else { panic!() };
    assert_eq!(own, f.root);
    let outside = plan(&f.root, &Place::Worktree(home.as_os_str())).unwrap_err();
    assert!(outside.contains("is not a worktree of this project"), "{outside}");

    // A new branch starts at the commit its base names, in the folder named or beside the project.
    let head = worktrees::commit(&f.root, "main").unwrap().unwrap();
    let Plan::New { path, branch: name, commit } = plan(&f.root, &branch("feature/x", Some("main"), None)).unwrap()
    else {
        panic!()
    };
    assert_eq!((path, name.as_str(), commit), (home.join("project-worktrees/feature-x"), "feature/x", Some(head)));
    let chosen = home.join("chosen");
    let Plan::New { path, .. } = plan(&f.root, &branch("feature/x", Some("main"), Some(chosen.as_os_str()))).unwrap()
    else {
        panic!()
    };
    assert_eq!(path, chosen);
    // A branch with no worktree gets one as it is, and takes no base.
    let Plan::New { commit, .. } = plan(&f.root, &branch("parked", None, None)).unwrap() else { panic!() };
    assert_eq!(commit, None);

    for (place, why) in [
        (
            branch("topic", Some("main"), None),
            format!("already checked out at {0}; to work there: --worktree {0}", linked.display()),
        ),
        (branch("main", None, None), "already checked out at".to_owned()),
        (branch("parked", Some("main"), None), "already exists; leave out --base".to_owned()),
        (branch("fresh", None, None), "new branch fresh needs --base REF".to_owned()),
        (branch("fresh", Some("nowhere"), None), "nowhere names no commit in this project".to_owned()),
        (branch("fresh", Some("--all"), None), "--all names no commit in this project".to_owned()),
        (branch("-b", Some("main"), None), "-b is not a branch name Git accepts".to_owned()),
        (branch("fresh", Some("main"), Some(linked.as_os_str())), "already exists; choose another --path".to_owned()),
        (branch("fresh", Some("main"), Some(OsStr::new("/tmp/a\nb"))), "control characters".to_owned()),
    ] {
        let refused = plan(&f.root, &place).unwrap_err();
        assert!(refused.contains(&why), "{place:?}: {refused}");
    }
    // Planning, and every refusal, left the project exactly as it was.
    assert_eq!(worktrees::list(&f.root).unwrap().len(), 2);
    assert!(!home.join("project-worktrees").exists() && !chosen.exists());
    assert_eq!(worktrees::has_branch(&f.root, "fresh"), Some(false));
    assert_eq!(worktrees::has_branch(&f.root, "feature/x"), Some(false));
}

#[test]
fn a_missing_agent_or_project_stops_the_launch_before_git_or_rex_is_asked_to_make_anything() {
    let (f, _) = project();
    let home = f.root.parent().unwrap().to_owned();
    let nowhere = SearchPath::with(OsString::new(), None);
    fn request<'a>(agent: &'a str, project: &'a Path) -> Request<'a> {
        let place = Place::Branch { name: "feature/x", base: Some("main"), path: None };
        Request { agent, place, prompt: Some("go on"), project: Some(project.as_os_str()), host: None }
    }
    for harness in &harness::HARNESSES {
        let refused = launch(&request(harness.id, &f.root), &nowhere).unwrap_err();
        let expected =
            format!("{} ({}) is not installed on this computer; nothing was created", harness.name, harness.id);
        assert_eq!(refused, expected);
    }
    let refused = launch(&request("codex", &home), &nowhere).unwrap_err();
    assert!(refused.ends_with("is not in a Git project; nothing was created"), "{refused}");
    assert!(!home.join("project-worktrees").exists());
    assert_eq!(worktrees::has_branch(&f.root, "feature/x"), Some(false));
}

#[test]
fn another_hosts_result_is_two_whole_lines_or_its_own_reason() {
    let id = "session:01a212ce-82f4-7ed0-bdb4-88aa2edd12fa";
    let screen = format!("Last login: today\nworktree /srv/app-worktrees/fix it\nsession {id}\n");
    assert_eq!(carried_result(&screen, true).unwrap(), format!("worktree /srv/app-worktrees/fix it\nsession {id}"));
    // A terminal can say anything. Only a real session ID after a success is a result.
    let forged = "worktree /srv/x\nsession session:../../etc\n";
    assert!(carried_result(forged, true).unwrap_err().contains("did not report a result"));
    assert!(carried_result(&screen, false).unwrap_err().contains("did not report a result"));
    let refused = "milo: Codex (codex) is not installed on this computer; nothing was created\n";
    assert_eq!(
        carried_result(refused, false).unwrap_err(),
        "Codex (codex) is not installed on this computer; nothing was created"
    );
    let coloured = format!("worktree /srv/\u{1b}[31mx\nsession {id}\n");
    assert!(!carried_result(&coloured, true).unwrap().contains('\u{1b}'));
}

#[test]
fn a_task_on_another_host_needs_folders_written_in_full_before_that_host_is_asked() {
    let ask = |project: Option<&'static str>, place| {
        let request = Request {
            agent: "claude",
            place,
            prompt: None,
            project: project.map(OsStr::new),
            host: Some(OsStr::new("devbox")),
        };
        on_host(OsStr::new("devbox"), &request).unwrap_err()
    };
    assert!(ask(None, branch("b", Some("main"), None)).contains("--host needs --project DIR"));
    let partial = "write each folder in full";
    assert!(ask(Some("app"), branch("b", Some("main"), None)).contains(partial));
    assert!(ask(Some("/srv/app"), Place::Worktree(OsStr::new("../other"))).contains(partial));
    assert!(ask(Some("/srv/app"), branch("b", Some("main"), Some(OsStr::new("trees/b")))).contains(partial));
}

#[cfg(unix)]
#[test]
fn the_action_is_told_the_installed_agents_and_the_projects_worktrees_and_branches() {
    let (f, linked) = project();
    let bin = f.root.parent().unwrap().join("bin");
    crate::format::tests::stand_in(&bin, "omp", "exit 0");
    crate::format::tests::stand_in(&bin, "claude", "exit 0");
    let search = SearchPath::with(bin.into_os_string(), None);
    let text = options_text(&linked.join("."), &search).unwrap();
    let path = |dir: &Path| hex(dir.as_os_str().as_encoded_bytes());
    let expected = format!(
        "project {linked}\nagent claude Claude Code\nagent omp oh-my-pi\nworktree {main} {main_branch} -\n\
         worktree {linked} {topic} here\nbranch {parked}\nbase {topic}\n",
        linked = path(&linked),
        main = path(&f.root),
        main_branch = hex(b"main"),
        topic = hex(b"topic"),
        parked = hex(b"parked"),
    );
    assert_eq!(text, expected);
    let outside = options_text(f.root.parent().unwrap(), &search).unwrap_err();
    assert_eq!(outside, "This shell is not in a Git project.");
}
