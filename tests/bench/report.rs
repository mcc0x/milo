//! What the timing tests share: which builds to time, their samples, and
//! the tab-separated report with a base build's medians beside the timed one's.
//!
//! - `MILO_TEST_BINARY` times that build instead of the one Cargo just made.
//! - `MILO_BENCH_BASE` also times that build, turn about with the first, and
//!   reports each median beside it.
//! - `MILO_BENCH_MAX_RATIO`, with a base, fails the test when a median is more
//!   than that many times the base's and also slower by more than the noise
//!   the test allows for that measure.
//! - `MILO_BENCH_RUNS` sets how often each scenario is started (10).
//! - `MILO_BENCH_REPORT` names a file to write the report to as well.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// One thing timed: a scenario and what was waited for in it.
type Measure = (&'static str, &'static str);

/// One build's times, in the order their measures were first timed.
#[derive(Default)]
pub struct Samples(Vec<(Measure, Vec<Duration>)>);

impl Samples {
    pub fn push(&mut self, scenario: &'static str, measure: &'static str, took: Duration) {
        let key = (scenario, measure);
        match self.0.iter_mut().find(|(known, _)| *known == key) {
            Some((_, samples)) => samples.push(took),
            None => self.0.push((key, vec![took])),
        }
    }

    fn median(&self, measure: Measure) -> Duration {
        let mut sorted = self.0.iter().find(|(known, _)| *known == measure).expect("timed in both builds").1.clone();
        sorted.sort_unstable();
        sorted[sorted.len() / 2]
    }
}

fn version(binary: &Path) -> String {
    let output = Command::new(binary).arg("--version").output().unwrap_or_else(|e| panic!("{}: {e}", binary.display()));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn named(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable).filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// What the environment asks for.
pub struct Bench {
    binary: PathBuf,
    base: Option<PathBuf>,
    pub runs: usize,
    max_ratio: Option<f64>,
}

impl Bench {
    /// `built` is the Milo that Cargo made for the test named `test`.
    pub fn from_env(built: &str, test: &str) -> Self {
        let binary = named("MILO_TEST_BINARY").unwrap_or_else(|| {
            if cfg!(debug_assertions) {
                panic!("time a release build: cargo test --release --test {test} -- --ignored");
            }
            built.into()
        });
        let number = |variable: &str| std::env::var(variable).ok();
        Self {
            binary,
            base: named("MILO_BENCH_BASE"),
            runs: number("MILO_BENCH_RUNS").map_or(10, |runs| runs.parse().expect("a number of runs")),
            max_ratio: number("MILO_BENCH_MAX_RATIO").map(|ratio| ratio.parse().expect("a ratio such as 1.25")),
        }
    }

    /// The builds to time in this run, each with its place in the pair of
    /// samples: the timed build's, then the base's. They take turns going
    /// first, so neither always runs on a warmer machine.
    pub fn builds(&self, run: usize) -> impl Iterator<Item = (&Path, usize)> {
        let mut order = [(Some(&self.binary), 0), (self.base.as_ref(), 1)];
        if run % 2 == 1 {
            order.reverse();
        }
        order.into_iter().filter_map(|(binary, build)| Some((binary?.as_path(), build)))
    }

    /// The first lines of a report: what was timed, in `what`'s words.
    pub fn heading(&self, what: &str) -> String {
        let Self { binary, base, runs, .. } = self;
        let mut text = format!("# {what}, {runs} runs, microseconds\n");
        writeln!(text, "# {} {}; timed: {}", std::env::consts::OS, std::env::consts::ARCH, version(binary)).unwrap();
        if let Some(base) = base {
            writeln!(text, "# base: {}", version(base)).unwrap();
        }
        text
    }

    /// Prints the report under `heading`, writes it where asked, and fails
    /// when a measure is slower than the base allows. `noise` is how much
    /// slower a measure's median may be whatever the ratio.
    pub fn report(&self, heading: String, [timed, based]: &[Samples; 2], noise: fn(&str) -> Duration) {
        let mut slower = Vec::new();
        let mut report = heading + "scenario\tmeasure\tsamples\tmedian\tmin\tmax";
        report.push_str(if self.base.is_some() { "\tbase_median\tratio\n" } else { "\n" });
        for (measure, samples) in &timed.0 {
            let (scenario, name) = *measure;
            let (min, max) = (samples.iter().min().unwrap(), samples.iter().max().unwrap());
            let median = timed.median(*measure);
            let columns = [samples.len() as u128, median.as_micros(), min.as_micros(), max.as_micros()];
            write!(report, "{scenario}\t{name}").unwrap();
            columns.iter().for_each(|column| write!(report, "\t{column}").unwrap());
            if self.base.is_some() {
                let before = based.median(*measure);
                let ratio = median.as_secs_f64() / before.as_secs_f64();
                write!(report, "\t{}\t{ratio:.2}", before.as_micros()).unwrap();
                if self.max_ratio.is_some_and(|max| ratio > max) && median > before + noise(name) {
                    slower.push(format!("{scenario} {name}: {median:?} against {before:?}"));
                }
            }
            report.push('\n');
        }
        print!("{report}");
        if let Some(path) = named("MILO_BENCH_REPORT") {
            std::fs::write(path, &report).unwrap();
        }
        assert!(slower.is_empty(), "slower than the base allows: {slower:#?}");
    }
}
