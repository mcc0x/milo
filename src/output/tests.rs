//! Checks which mentions in terminal output are offered, in what order and with what beside them.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;

use super::*;
use crate::fixture::Fixture;

/// A Git worktree with a few files, and a file beside it that is not the worktree's.
fn project() -> (Fixture, Worktree) {
    let f = Fixture::new();
    f.write("src/reader/disk.rs", "fn save() {}\n");
    f.write("src/main.rs", "fn main() {}\n");
    f.write("docs/migration.md", "# Migration\n");
    f.write("app/(site)/[slug]/page.tsx", "export default 1\n");
    f.write("Makefile", "all:\n");
    fs::write(f.root.parent().unwrap().join("secret.txt"), "outside\n").unwrap();
    let tree = Worktree::of(&f.root).unwrap();
    (f, tree)
}

fn titles(report: &Report) -> Vec<String> {
    report.found.iter().map(|(place, _)| place.title()).collect()
}

#[test]
fn a_compiler_location_is_one_place_with_its_diagnostic_beside_it() {
    let (_f, tree) = project();
    let output = "   Compiling milo v0.1.0\nerror[E0308]: mismatched types\n   --> src/reader/disk.rs:184:17\n    |\n";
    let report = scan(output, &tree);
    assert_eq!(report.listing(), "found 1 outside 0\nsrc/reader/disk.rs:184:17\terror[E0308]: mismatched types\n");
}

#[test]
fn a_file_an_agent_mentions_without_a_line_is_offered_with_what_was_said() {
    let (_f, tree) = project();
    let report = scan("⏺ I updated `docs/migration.md`, which now covers the rename.\n", &tree);
    assert_eq!(
        report.listing(),
        "found 1 outside 0\ndocs/migration.md\t⏺ I updated `docs/migration.md`, which now covers the rename.\n"
    );
}

#[test]
fn recent_mentions_come_first_and_only_a_repeated_place_is_collapsed() {
    let (_f, tree) = project();
    let output = "src/main.rs:1:1: warning: old\n\
                  src/reader/disk.rs:1:4: error: first\n\
                  src/main.rs:1:1: warning: again\n\
                  src/reader/disk.rs:1:9: error: second\n\
                  see src/reader/disk.rs for both\n";
    let report = scan(output, &tree);
    assert_eq!(
        report.listing(),
        "found 4 outside 0\n\
         src/reader/disk.rs\tsee src/reader/disk.rs for both\n\
         src/reader/disk.rs:1:9\tsrc/reader/disk.rs:1:9: error: second\n\
         src/main.rs:1:1\tsrc/main.rs:1:1: warning: again\n\
         src/reader/disk.rs:1:4\tsrc/reader/disk.rs:1:4: error: first\n"
    );
}

#[test]
fn the_ways_tools_print_a_place_are_read() {
    let (f, tree) = project();
    let absolute = f.root.join("src/main.rs");
    let cases = [
        ("src/main.rs:12", "src/main.rs:12"),
        ("src/main.rs:12:fn main() {", "src/main.rs:12"),
        ("src/main.rs(12,3): error TS2304", "src/main.rs:12:3"),
        ("  File \"src/main.rs\", line 7, in <module>", "src/main.rs:7"),
        ("    at run (src/main.rs:3:9)", "src/main.rs:3:9"),
        ("[notes](docs/migration.md#L4)", "docs/migration.md:4"),
        ("modified:   ./src/../Makefile", "Makefile"),
        ("app/(site)/[slug]/page.tsx:1:8", "app/(site)/[slug]/page.tsx:1:8"),
        ("--manifest-path=src/main.rs,", "src/main.rs"),
    ];
    for (output, expected) in cases {
        assert_eq!(titles(&scan(output, &tree)), [expected], "{output}");
    }
    let report = scan(&format!("{}:5:1: note", absolute.display()), &tree);
    assert_eq!(titles(&report), ["src/main.rs:5:1"]);
}

#[cfg(unix)]
#[test]
fn what_is_not_a_file_of_the_worktree_is_never_offered() {
    let (f, tree) = project();
    f.write("vendor/lib/.git/HEAD", "ref: refs/heads/main\n");
    f.write("vendor/lib/lib.rs", "pub fn other() {}\n");
    symlink(f.root.parent().unwrap().join("secret.txt"), f.root.join("link.txt")).unwrap();
    let output = "../secret.txt:1 /etc/hosts link.txt vendor/lib/lib.rs:1 .git/HEAD\n\
                  https://example.com/src/main.rs file:///etc/hosts ~/src/main.rs\n\
                  src missing/file.rs v1.2.3 e.g. main\n";
    let report = scan(output, &tree);
    assert_eq!(report.listing(), "found 0 outside 5\n");
    for refused in ["../secret.txt", "/etc/hosts:3", "link.txt", "vendor/lib/lib.rs", "src", "nowhere.rs"] {
        assert_eq!(tree.place(refused), None, "{refused}");
    }
}

#[test]
fn relative_paths_start_from_the_working_folder_and_stop_at_the_worktree() {
    let (f, _) = project();
    let tree = Worktree::of(&f.root.join("src")).unwrap();
    assert_eq!(tree.root, crate::sys::canonicalize(&f.root).unwrap());
    let report = scan("main.rs:1 ../docs/migration.md ../../secret.txt src/main.rs\n", &tree);
    assert_eq!((titles(&report), report.outside), (vec!["src/main.rs:1".to_owned(), "docs/migration.md".into()], 1));

    // Outside Git, the working folder itself is the limit.
    let plain = Fixture::folder();
    plain.write("sub/note.txt", "note\n");
    plain.write("top.txt", "top\n");
    let tree = Worktree::of(&plain.root.join("sub")).unwrap();
    let report = scan("note.txt ../top.txt\n", &tree);
    assert_eq!((titles(&report), report.outside), (vec!["note.txt".to_owned()], 1));
    assert!(Worktree::of(Path::new("relative")).is_none());
    assert!(Worktree::of(&plain.root.join("top.txt")).is_none());
}

#[test]
fn only_the_end_of_long_output_is_looked_at_and_its_text_is_made_plain() {
    let (_f, tree) = project();
    let mut output = String::from("src/main.rs:1: too old to matter\n");
    output.push_str(&"filler\n".repeat(LINES));
    assert_eq!(scan(&output, &tree).listing(), "found 0 outside 0\n");

    let long = format!("\u{1b}[31merror\u{1b}[0m:\t{} src/main.rs:2 \u{7}bell", "word ".repeat(60));
    let report = scan(&long, &tree);
    let context = &report.found[0].1;
    assert!(context.starts_with("error: word word") && context.ends_with('…'), "{context}");
    assert!(context.chars().count() <= CONTEXT + 1 && !context.contains(|c: char| c.is_control()), "{context}");

    let many: String = (1..=OFFERED + 50).map(|line| format!("src/main.rs:{line}: note\n")).collect();
    assert_eq!(scan(&many, &tree).found.len(), OFFERED);
}

#[test]
fn a_place_goes_to_the_first_reader_that_takes_it_or_to_a_new_one() {
    let (f, tree) = project();
    let root = f.root.canonicalize().unwrap();
    let blocks: Vec<OsString> =
        ["block:11111111-1111-1111-1111-111111111111", "reader", "block:22222222-2222-2222-2222-222222222222"]
            .into_iter()
            .map(OsString::from)
            .collect();
    let place = tree.place("src/reader/disk.rs:184:17").unwrap();
    let asked = std::cell::RefCell::new(Vec::new());
    let second_takes = |block: &str, project: &Path, file: &Path, spot: Option<Spot>| {
        asked.borrow_mut().push((block.to_owned(), project.to_owned(), file.to_owned(), spot));
        block.starts_with("block:2")
    };
    assert_eq!(
        tree.open(&place, &blocks, second_takes).unwrap(),
        "shown\tblock:22222222-2222-2222-2222-222222222222\tdisk.rs\n"
    );
    // Each is asked about this worktree's file. A pane named by a label is never asked.
    let spot = Some(Spot { line: 184, column: Some(17) });
    let asked = asked.into_inner();
    assert_eq!(asked.len(), 2);
    for (index, block) in [(0, &blocks[0]), (1, &blocks[2])] {
        let expected = (block.to_str().unwrap().to_owned(), root.clone(), root.join("src/reader/disk.rs"), spot);
        assert_eq!(asked[index], expected);
    }

    let file = root.join("docs/migration.md");
    let place = tree.place("docs/migration.md").unwrap();
    assert_eq!(
        tree.open(&place, &blocks, |_, _, _, _| false).unwrap(),
        format!("view\t{}\t{}\t-\tmigration.md\n", root.display(), file.display())
    );
}
