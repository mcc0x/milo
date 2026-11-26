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
    let project = std::env::current_dir().unwrap().join("project");
    let command = format!(r#""{}" history"#, env!("CARGO_BIN_EXE_milo"));
    let mut console = Console::start(&command, &project, &env);
    if let Ok(choice) = std::env::var("MILO_TEST_CHOICE") {
        console.see("Anonymous usage counts", 0);
        console.see("turn off", 0);
        console.keys(if choice == "off" { b"o" } else { b"\r" });
    }
    console.see(&std::env::var("MILO_TEST_FRAME").unwrap(), 0);
    println!("installed pane ready");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
    console.keys(b"q");
    assert_eq!(console.exit_code(Duration::from_secs(10)), Some(0));
}
