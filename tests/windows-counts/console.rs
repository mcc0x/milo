// Compiled alongside the native Windows console fixture with CARGO_BIN_EXE_milo
// pointing to the installed executable rather than Cargo's build directory.
#[test]
fn installed_counts_pane() {
    let profile = PathBuf::from(std::env::var_os("MILO_TEST_PROFILE").unwrap());
    let env = vec![
        ("USERPROFILE".into(), profile.clone()),
        ("APPDATA".into(), profile.join("Roaming")),
        ("LOCALAPPDATA".into(), profile.join("Local")),
    ];
    let work = std::env::current_dir().unwrap();
    let project = work.join("project");
    let binary = env!("CARGO_BIN_EXE_milo");
    let command = match std::env::var("MILO_TEST_CONFINED_PYTHON") {
        Ok(python) => format!(r#""{python}" "{}" "{binary}""#, work.join("no-helpers.py").display()),
        Err(_) => format!(r#""{binary}" history"#),
    };
    let mut console = Console::start(&command, &project, &env);
    console.see(&std::env::var("MILO_TEST_FRAME").unwrap(), 0);
    println!("installed pane ready");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
    console.keys(b"q");
    assert_eq!(console.exit_code(Duration::from_secs(10)), Some(0));
    let output = console.text().to_lowercase();
    for label in ["anonymous usage", "milo_telemetry", "usage counts", "keep on", "turn off", "telemetry"] {
        assert!(!output.contains(label), "unexpected counts text {label:?} in the terminal");
    }
}
