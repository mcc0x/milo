use super::*;

/// Model a focused New Tab request at the server boundary; app.lua owns and measures
/// the new shell. Neither this client nor the lab's measuring client is the macOS app.
fn focused_tab(lab: &Lab, session: &str, dir: &Path) -> (String, String) {
    let request = format!(
        r#"{{"session_id":"{session}","layout":{{"block":{{"flavor":"com.superlogical.terminal.shell","options":{{"cwd":"{}"}}}}}}}}"#,
        dir.display()
    );
    let made = lab.rex(&["api", "call", "session.new_window", &request]);
    (id(&made, "window_id"), id(&made, "block_ids"))
}

fn arranged(lab: &Lab, tab: &str, dir: &Path, programs: &[&str], left: &str) -> Tab {
    let began = Instant::now();
    loop {
        let tab = lab.view().tabs.remove(tab).unwrap();
        let actual: Vec<_> = tab
            .panes
            .iter()
            .map(|pane| {
                if pane.program == "milo" && Path::new(&pane.dir) == dir { pane.label.as_str() } else { &pane.program }
            })
            .collect();
        if actual == programs && tab.left == left {
            return tab;
        }
        assert!(began.elapsed() < PATIENCE, "expected {programs:?} with left share {left}, got {tab:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "needs an installed Rex binary"]
fn new_tab_keeps_the_saved_width_when_rex_defaults_the_outer_split() {
    let Scene { lab, first, second, session } = Scene::start();
    for (dir, profile) in [(&first, "v1 l 0.214686 0.70 2 ohu 100"), (&second, "v1 r 0.270000 0.70 2 ohu 110")] {
        lab.milo(&["layout-profile", "save", "worktree", dir.to_str().unwrap(), profile]);
    }
    let left = ["milo:outline", "shell"];
    let cli = lab.new_tab(&session, &first);
    arranged(&lab, &cli, &first, &left, "0.214686");
    let (control, _) = focused_tab(&lab, &session, &first);
    arranged(&lab, &control, &first, &left, "0.214686");

    // Without the native app both paths honor the ratio. Reproduce #121's server
    // state by dropping only that input, not by resizing a pane after creation.
    let shim = format!(
        r#"
local real = rex.session
local session = setmetatable({{}}, {{__index = real}})
function session.new_split(p)
  if p.direction == "horizontal" then
    local log = assert(io.open("{}/omitted", "a"))
    log:write(string.format("%.6f\n", p.ratio))
    log:close()
    p.ratio = nil
  end
  return real.new_split(p)
end
local rex = setmetatable({{session = session}}, {{__index = rex}})
"#,
        lab.root.display()
    );
    let config = lab.root.join("home/.config/milo/rex.lua");
    fs::write(&config, shim + &fs::read_to_string(&config).unwrap()).unwrap();

    let (app, shell) = focused_tab(&lab, &session, &first);
    let tab = arranged(&lab, &app, &first, &left, "0.214686");
    assert_eq!(tab.panes[1].block, shell);

    let (right, shell) = focused_tab(&lab, &session, &second);
    let tab = arranged(&lab, &right, &second, &["shell", "milo:outline", "milo:history"], "0.730000");
    assert_eq!(tab.panes[0].block, shell);
    lab.drag_rail(&session, lab.view().tab(&app), "0.31");
    lab.remembers(&first, "0.310000");
    let (again, _) = focused_tab(&lab, &session, &first);
    arranged(&lab, &again, &first, &left, "0.310000");
    assert_eq!(lab.view().tab(&app).left, "0.310000");
    assert_eq!(lab.remembered_width(&second), "0.270000");
    assert_eq!(lab.read("omitted"), "0.214686\n0.730000\n0.310000\n");
}
