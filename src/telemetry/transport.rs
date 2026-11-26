use std::process::{Child, Command, Stdio};

/// A build may name one HTTPS receiver or a literal loopback test server.
/// Neither the inherited environment nor the checkout can redirect a request.
pub(super) fn protocol(url: &str) -> Option<&'static str> {
    let origin = url.strip_suffix("/v1/counts")?;
    if let Some(host) = origin.strip_prefix("https://") {
        return (!host.is_empty()
            && host.split('.').all(|label| {
                !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
        .then_some("=https");
    }
    let port = origin.strip_prefix("http://127.0.0.1:")?;
    (port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().ok()? > 0).then_some("=http")
}

pub(super) fn start(url: &str, date: &str, new_install: bool, monthly_active: bool) -> Option<Child> {
    let body = format!(r#"{{"day":"{date}","new_install":{new_install},"monthly_active":{monthly_active}}}"#);
    send(super::platform::curl()?, url, &body)
}

fn send(mut command: Command, url: &str, body: &str) -> Option<Child> {
    crate::fs_ops::spawn(
        command
            .args([
                "-q",
                "--no-ssl-auto-client-cert",
                "--proto",
                protocol(url)?,
                "--noproxy",
                "*",
                "--globoff",
                "--http1.1",
                "--retry",
                "0",
                "--max-time",
                "3",
                "--max-filesize",
                "1024",
                "--user-agent",
                "",
                "--header",
                "Content-Type: application/json",
                "--data-binary",
                body,
                "--url",
                url,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_invalid_executable_drops_the_attempt() {
        let f = crate::fixture::Fixture::folder();
        let path = f.root.join("curl.exe");
        let local = "http://127.0.0.1:1/v1/counts";
        assert!(send(Command::new(&path), local, "{}").is_none());
        f.write("curl.exe", "Not an executable.");
        assert!(send(Command::new(&path), local, "{}").is_none());
    }
}
