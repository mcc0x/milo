# GitHub Actions routing

Both workflows report checks on every pull request. On ordinary PRs into `main`, the two Linux selectors inspect the full tree diff from the base parent to GitHub's test merge commit. They require the expected base and head parents and only skip jobs for explicit, known paths. A missing or empty diff, an unknown path, or a workflow change selects all jobs. The documentation bundle check always runs.

The public website and docs normally run without the app builds. The hosted counts service runs the counts and dashboard/relay checks. Shared Rust and embedded dashboard inputs keep all native, portable, counts and Rex coverage; a platform-only installer or fixture keeps that platform's packaged checks. Watch for cross-directory inputs: the public Mac installer, menu artwork and shared relay message list are also app inputs. Update `pr_impact.py` and its fixtures when adding a new dependency between lanes.

New runs only replace older runs in the same workflow and ordinary PR. Main pushes, manual dispatches, explicit reruns, and PRs from `release/`, `qa/` or `qualify/` refs aren't canceled by this policy. Main pushes retain their existing Mac path check; manual release preparation still runs the full suite. The `Apple Silicon release tests` context belongs to main or an explicit dispatch on the source SHA, not a PR or a path-based skip.

Run `python3 -B -m unittest discover -s .github/ci -p test_pr_impact.py` and `actionlint` to verify changes to the routing.
