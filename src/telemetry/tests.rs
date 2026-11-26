use super::*;
use crate::fixture::Fixture;
use std::io::{BufRead as _, BufReader, Read as _};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn enter(root: &Path) -> io::Result<Visit> {
    Visit::enter(root.join("config"), root.join("state"))
}

#[test]
fn the_build_settings_choose_only_an_authorized_endpoint() {
    let url = option_env!("MILO_COUNTS_ENDPOINT");
    assert_eq!(endpoint(), url.filter(|value| crate::usage::counts_protocol(value).is_some()));
}

#[test]
fn utc_dates_agree_across_centuries_and_reject_corrupt_values() {
    for (day, expected) in [
        (0, "1970-01-01"),
        (11_016, "2000-02-29"),
        (47_541, "2100-03-01"),
        (146_097, "2370-01-01"),
        (2_932_896, "9999-12-31"),
    ] {
        assert_eq!(utc_date(day).as_deref(), Some(expected));
    }
    for invalid in [-1, 2_932_897, i64::MAX] {
        assert!(utc_date(invalid).is_none());
    }
}

#[test]
fn first_seen_is_recorded_without_a_report_and_kept_across_settings_changes() {
    let f = Fixture::folder();
    let first = enter(&f.root).unwrap();
    assert_eq!(first.previous_or_mark_first().unwrap(), None);
    let path = first.state.join(HIGH_WATER);
    assert_eq!(fs::read_to_string(&path).unwrap(), FIRST_SEEN);
    set_enabled(&first.config, false).unwrap();
    assert!(enter(&f.root).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), FIRST_SEEN);
    set_enabled(&first.config, true).unwrap();
    assert!(enter(&f.root).is_err(), "the first session still holds the lock");
    let _inherited_before_exec = first._guard.0.try_clone().unwrap();
    drop(first);
    assert_eq!(enter(&f.root).unwrap().previous_or_mark_first().unwrap().as_deref(), Some(FIRST_SEEN));
}

fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(error) => panic!("no loopback request: {error}"),
        }
    }
}

fn request(listener: &TcpListener, mut child: Child) -> (String, String) {
    let mut socket = accept(listener);
    socket.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
    let mut reader = BufReader::new(&socket);
    let mut head = String::new();
    while !head.ends_with("\r\n\r\n") {
        assert!(reader.read_line(&mut head).unwrap() > 0 && head.len() < 1024);
    }
    let length =
        head.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ")?.parse().ok()).unwrap();
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    socket.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    assert!(child.wait().unwrap().success());
    (head, String::from_utf8(body).unwrap())
}

#[test]
fn bounded_wire_contract_lost_reports_rollbacks_and_calendar_boundaries() {
    let f = Fixture::folder();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/counts", server.local_addr().unwrap());
    assert_eq!(enter(&f.root).unwrap().previous_or_mark_first().unwrap(), None);
    for (now, expected) in [
        (1_709_164_799, Some(("2024-02-28", true, true))),
        (1_709_164_799, None),
        (1_709_164_800, Some(("2024-02-29", false, false))),
        (1_709_251_199, None),
        (1_709_251_200, Some(("2024-03-01", false, true))),
        (1_700_000_000, None),
        (1_730_000_000, Some(("2024-10-27", false, true))),
    ] {
        let visit = enter(&f.root).unwrap();
        let attempt = visit.report(&visit.previous_or_mark_first().unwrap().unwrap(), now, &url);
        if let Some((date, new, month)) = expected {
            assert_eq!(
                fs::read_to_string(visit.state.join(HIGH_WATER)).unwrap(),
                format!("{}\n", now / crate::when::DAY)
            );
            let (head, body) = request(&server, attempt.unwrap());
            assert_eq!(body, format!(r#"{{"day":"{date}","new_install":{new},"monthly_active":{month}}}"#));
            assert_eq!(head.lines().next(), Some("POST /v1/counts HTTP/1.1"));
            for line in head.lines().skip(1).filter(|line| !line.is_empty()) {
                let name = line.split_once(':').unwrap().0.to_ascii_lowercase();
                assert!(["host", "content-type", "content-length", "accept"].contains(&name.as_str()), "{name}");
            }
        } else {
            assert!(attempt.is_none());
        }
        set_enabled(&visit.config, false).unwrap();
        assert!(visit.report(&visit.previous_or_mark_first().unwrap().unwrap(), 1_900_000_000, &url).is_none());
        set_enabled(&visit.config, true).unwrap();
    }
    server.set_nonblocking(true).unwrap();
    assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn overlapping_processes_skip_first_session_and_only_one_claims_afterwards() {
    const NAME: &str = "telemetry::tests::overlapping_processes_skip_first_session_and_only_one_claims_afterwards";
    const ROOT: &str = "MILO_TEST_COUNTS_DIR";
    if let Some(root) = std::env::var_os(ROOT) {
        let port: u16 = std::env::var("MILO_TEST_COUNTS_PORT").unwrap().parse().unwrap();
        if let Ok(visit) = enter(Path::new(&root))
            && let Some(last) = visit.previous_or_mark_first().unwrap()
            && let Some(mut child) = visit.report(&last, 1_730_000_000, &format!("http://127.0.0.1:{port}/v1/counts"))
        {
            child.wait().unwrap();
        }
        return;
    }
    let f = Fixture::folder();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", NAME])
        .env(ROOT, &f.root)
        .env("MILO_TEST_COUNTS_PORT", server.local_addr().unwrap().port().to_string())
        .stdout(Stdio::null());
    let first = enter(&f.root).unwrap();
    assert_eq!(first.previous_or_mark_first().unwrap(), None);
    assert!(command.status().unwrap().success());
    assert_eq!(fs::read_to_string(first.state.join(HIGH_WATER)).unwrap(), FIRST_SEEN);
    drop(first);
    let mut children: Vec<_> = (0..8).map(|_| command.spawn().unwrap()).collect();
    let mut socket = accept(&server);
    socket.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").unwrap();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(
        fs::read_to_string(f.root.join("state").join(HIGH_WATER)).unwrap(),
        format!("{}\n", 1_730_000_000 / crate::when::DAY)
    );
    server.set_nonblocking(true).unwrap();
    assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn corrupt_or_untrusted_state_and_endpoints_fail_closed() {
    for url in [
        "",
        "http://example.test/v1/counts",
        "https://me@host/v1/counts",
        "https://host/../v1/counts",
        "http://127.0.0.1:80/v1/counts#frag",
        "http://127.0.0.2:80/v1/counts",
        "file:///v1/counts",
        "https://example.test/v1/counts?key=bad",
        "https://{host1,host2}/v1/counts",
    ] {
        assert!(crate::usage::counts_protocol(url).is_none(), "{url}");
    }
    assert_eq!(
        crate::usage::counts_protocol("https://counts.example.test/v1/counts"),
        cfg!(milo_distribution_counts).then_some("=https"),
    );
    assert_eq!(crate::usage::counts_protocol("http://127.0.0.1:12345/v1/counts"), Some("=http"));
    assert!(enabled(Path::new("relative")).is_err());
    let f = Fixture::folder();
    let visit = enter(&f.root).unwrap();
    f.write("state/anonymous-counts.state", "broken");
    assert!(visit.report("broken", 1_730_000_000, "http://127.0.0.1:1/v1/counts").is_none());
    assert_eq!(fs::read_to_string(visit.state.join(HIGH_WATER)).unwrap(), "broken");
    f.write("config/anonymous-counts.conf", "unexpected");
    assert!(enabled(&visit.config).is_err());
}

#[test]
fn legacy_first_seen_state_remains_eligible_and_reports_only_once() {
    let f = Fixture::folder();
    f.write("state/anonymous-counts.state", "noticed\n");
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/counts", server.local_addr().unwrap());
    let visit = enter(&f.root).unwrap();
    let previous = visit.previous_or_mark_first().unwrap().unwrap();
    let now = 1_730_000_000;
    let (_, body) = request(&server, visit.report(&previous, now, &url).unwrap());
    assert_eq!(body, r#"{"day":"2024-10-27","new_install":true,"monthly_active":true}"#);
    drop(visit);
    let visit = enter(&f.root).unwrap();
    assert!(visit.report(&visit.previous_or_mark_first().unwrap().unwrap(), now, &url).is_none());
}
