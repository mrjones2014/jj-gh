//! `jj-gh pr rerun --watch`: babysit CI until you quit.
//!
//! Polls the CI status of the selected PRs on an interval and retries
//! retryable failures as they happen, instead of the one-shot retry of plain
//! `pr rerun`. Selection composes with the usual flags, with two differences:
//! a bare `--watch` implies `--all`, and the all-PRs selection watches every
//! local open PR, including currently-green ones (they can go red on a new
//! push). The selection is re-resolved every tick, so PRs created while
//! watching are picked up; merged and closed PRs stay listed with a
//! checkmark and are left alone.
//!
//! Per tick, the cheap `statusCheckRollup` search decides which PRs need a
//! closer look; only those get the per-SHA workflow-runs REST call. Completed
//! runs with a retryable failure are retried (full restart with `--all-jobs`),
//! without limit by default; with `--max-retries`, a run that keeps failing
//! past the budget is marked given-up and left alone. With `--cancel`,
//! in-progress runs that already contain a failed job are cancelled; unlike
//! the one-shot path this never blocks on the cancellation, the run simply
//! comes back as a retryable failure on a later tick. `--cancel --all-jobs`
//! keeps the failed-job definition of "doomed" here: cancelling every
//! in-progress run on every tick would kill the very reruns this loop starts.
//!
//! On a TTY this renders an inline list (one row per PR) with `j`/`k`/`g`/`G`
//! to move, `p` to pause/resume retrying the selected PR (watched but left
//! alone, marked with a pause icon), `R` to force a retry of the selected PR
//! right now (cancel + rerun, even mid-run, bypassing the retry budget and
//! any pause), and `q`/Esc/Ctrl-C to quit.
//! Without a TTY it logs one line per event instead and runs until killed,
//! with a spinner status line on stderr when that is a terminal.

use crate::{
    cli::GlobalOpts,
    commands::pr::rerun::{RerunArgs, RerunMode, first_failed_job, split_completed},
    gh::{CheckCounts, CiStatus, Gh, PrWithCiStatus, WorkflowRun, remote::Target},
    jj::Jj,
    model::{LocalPulls, Model},
    ui::{
        Hyperlink as _, Stream,
        status::SPINNER_FRAMES,
        tui::{
            InlineSession, ListFrame, ListNav, ListState, VERTICAL_SEPARATOR, bounded_region_rows,
            handle_list_nav, render_list_rows, truncate,
        },
    },
    util::EvalWithCfgFallback,
};
use anyhow::{Context, Result};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    queue,
    style::{self, Stylize as _},
    terminal,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{IsTerminal, Write as _};
use std::time::Duration;

/// Ticks a just-retried run may keep reporting its stale completed/failed
/// state before we treat that as a fresh failure and retry again.
const RESTART_GRACE_TICKS: u8 = 2;

pub(super) async fn run(model: &impl Model, args: &RerunArgs) -> Result<()> {
    let RerunArgs {
        revsets,
        all,
        cancel,
        all_jobs,
        cancel_timeout: _,
        watch: _,
        poll_interval,
        max_retries,
        globals:
            GlobalOpts {
                remote,
                upstream_remote,
                verbose: _,
                quiet: _,
                log_level: _,
                gh_askpass: _,
                askpass_timeout_secs: _,
            },
    } = args;
    let upstream_remote = crate::gh::remote::resolved_upstream_remote(upstream_remote);
    let mut watcher = Watcher::new(
        Selection::from_args(revsets, *all),
        WatchOpts {
            mode: RerunMode {
                cancel: *cancel,
                all_jobs: *all_jobs,
            },
            max_retries: *max_retries,
        },
    );
    let interval = Duration::from_secs(*poll_interval);
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        run_interactive(model, &mut watcher, remote, upstream_remote, interval).await
    } else {
        run_plain(model, &mut watcher, remote, upstream_remote, interval).await
    }
}

/// How the watched set is chosen, re-resolved on every tick.
///
/// Deliberately looser than the one-shot
/// [`crate::gh::pr_lookup::resolve_pr_targets`] rules: a revset matching no
/// commits is not an error (matches may appear while watching), and every
/// bookmark of a matched commit is considered, not just the first, since a
/// watcher wants the superset of PRs that could go red.
enum Selection {
    /// Every local open PR, including green ones.
    All,
    /// PRs named by number (static) plus PRs matching the revsets
    /// (re-resolved each tick, so new matches get picked up).
    Explicit {
        numbers: Vec<u64>,
        revsets: Vec<String>,
    },
}

impl Selection {
    fn from_args(args: &[String], all: bool) -> Self {
        if all || args.is_empty() {
            return Self::All;
        }
        let mut numbers = Vec::new();
        let mut revsets = Vec::new();
        for arg in args {
            if let Ok(number) = arg.parse::<u64>() {
                numbers.push(number);
            } else {
                revsets.push(arg.clone());
            }
        }
        Self::Explicit { numbers, revsets }
    }
}

struct WatchOpts {
    mode: RerunMode,
    /// Per-run automatic retry budget; `None` means unlimited.
    max_retries: Option<u32>,
}

/// Per-run bookkeeping, keyed by run ID. A new push produces new run IDs, so
/// the retry budget resets naturally on new heads.
#[derive(Default)]
struct RunState {
    /// Automatic retries triggered on this run so far.
    retries: u32,
    /// Set right after a retry; while nonzero, a stale completed/failed
    /// snapshot of this run is not treated as a fresh failure.
    restart_grace: u8,
    /// A cancellation was requested and has not been observed to land yet.
    cancel_requested: bool,
    /// Hit the retry budget; left alone until forced.
    given_up: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrDisplayStatus {
    Pending,
    Failed,
    Retrying,
    Green,
    /// No checks reported for the head yet: none configured, or a fresh push
    /// whose checks have not registered. Not green, but nothing to retry;
    /// counts as settled for the all-green announcement since it cannot fail.
    NoChecks,
    GivenUp,
    /// Merged or closed; kept in the list with a checkmark and left alone.
    Done,
}

struct PrState {
    number: u64,
    title: String,
    url: String,
    head_sha: String,
    /// Rolled-up CI status from the cheap search; `None` for explicit PR
    /// numbers that are not local PRs (their status comes from their runs).
    rollup: Option<CiStatus>,
    /// Check tallies from the same search; `None` when `rollup` is.
    check_counts: Option<CheckCounts>,
    status: PrDisplayStatus,
    /// Total retries triggered on this PR since the current head was pushed.
    retries: u32,
    /// `R` was pressed: next tick cancels in-progress runs and retries failed
    /// ones, bypassing the retry budget.
    force: bool,
    /// `p` was pressed: watching continues (status from the cheap rollup) but
    /// nothing is cancelled or retried until unpaused. `R` still acts.
    paused: bool,
    /// Found via the local-PR search. Disappearing from the open set, or
    /// flipping to a direct-lookup candidate (`local: false`), means merged or
    /// closed rather than merely deselected.
    local: bool,
    runs: HashMap<u64, RunState>,
}

impl PrState {
    fn new(candidate: &Candidate) -> Self {
        Self {
            number: candidate.number,
            title: candidate.title.clone(),
            url: candidate.url.clone(),
            head_sha: candidate.head_sha.clone(),
            rollup: candidate.rollup,
            check_counts: candidate.check_counts,
            status: PrDisplayStatus::Pending,
            retries: 0,
            force: false,
            paused: false,
            local: candidate.local,
            runs: HashMap::new(),
        }
    }
}

/// One PR the current tick's selection wants watched.
struct Candidate {
    number: u64,
    title: String,
    url: String,
    head_sha: String,
    rollup: Option<CiStatus>,
    check_counts: Option<CheckCounts>,
    local: bool,
}

impl Candidate {
    fn from_local(pr: &PrWithCiStatus) -> Self {
        Self {
            number: pr.number,
            title: pr.title.clone(),
            url: pr.url.clone(),
            head_sha: pr.head_sha.clone(),
            rollup: Some(pr.ci_status),
            check_counts: Some(pr.check_counts),
            local: true,
        }
    }
}

/// Something that happened during a tick. Logged as plain lines without a
/// TTY; the TUI reflects the same information through row state instead.
#[derive(Debug, PartialEq, Eq)]
enum WatchEvent {
    Added {
        number: u64,
        title: String,
        url: String,
    },
    ClosedOrMerged {
        number: u64,
    },
    Deselected {
        number: u64,
    },
    NewHead {
        number: u64,
    },
    CancelledRun {
        number: u64,
        run: u64,
    },
    RetriedRun {
        number: u64,
        run: u64,
        attempt: u32,
    },
    GaveUp {
        number: u64,
        run: u64,
        retries: u32,
    },
    Green {
        number: u64,
    },
    AllGreen,
    PrError {
        number: u64,
        message: String,
    },
}

struct Watcher {
    selection: Selection,
    opts: WatchOpts,
    prs: BTreeMap<u64, PrState>,
    all_green_announced: bool,
}

impl Watcher {
    fn new(selection: Selection, opts: WatchOpts) -> Self {
        Self {
            selection,
            opts,
            prs: BTreeMap::new(),
            all_green_announced: false,
        }
    }

    /// One poll cycle: re-resolve the selection, reconcile the watched set,
    /// then inspect and act on each PR. Per-PR failures become
    /// [`WatchEvent::PrError`] and do not stop the other PRs; failures
    /// resolving the selection itself fail the whole tick.
    async fn tick(
        &mut self,
        model: &impl Model,
        remote: &EvalWithCfgFallback<String>,
        upstream_remote: &str,
    ) -> Result<Vec<WatchEvent>> {
        let local = model.local_pulls(remote, Some(upstream_remote)).await?;
        let candidates = self.wanted(model, &local).await?;
        let gh = model.gh().await?;
        let mut events = Vec::new();
        self.sync_watched_set(&candidates, &local.prs, &mut events);
        for pr in self.prs.values_mut() {
            if pr.status == PrDisplayStatus::Done {
                continue;
            }
            let result = process_pr(gh, &local.target, pr, &self.opts).await;
            if let Ok(mut pr_events) = result.inspect_err(|e| {
                events.push(WatchEvent::PrError {
                    number: pr.number,
                    message: format!("{e:#}"),
                });
            }) {
                events.append(&mut pr_events);
            }
        }
        self.announce_all_green(&mut events);
        Ok(events)
    }

    /// Resolve the current selection into the PRs this tick should watch.
    async fn wanted(&self, model: &impl Model, local: &LocalPulls) -> Result<Vec<Candidate>> {
        let Selection::Explicit { numbers, revsets } = &self.selection else {
            return Ok(local.prs.iter().map(Candidate::from_local).collect());
        };
        let mut bookmarks = HashSet::new();
        for revset in revsets {
            for commit in model
                .jj()
                .resolve_revset(revset)
                .await
                .with_context(|| format!("resolving revset `{revset}`"))?
            {
                bookmarks.extend(commit.bookmarks);
            }
        }
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for pr in &local.prs {
            if numbers.contains(&pr.number) || bookmarks.contains(&pr.head_ref_name) {
                seen.insert(pr.number);
                out.push(Candidate::from_local(pr));
            }
        }
        // Explicit numbers that are not local PRs: fall back to a direct
        // lookup. No cheap rollup for these, so their runs are always
        // inspected. Numbers already retired as merged/closed are skipped so
        // a finished PR stops costing a lookup per tick.
        let gh = model.gh().await?;
        for number in numbers {
            if seen.contains(number)
                || self
                    .prs
                    .get(number)
                    .is_some_and(|pr| pr.status == PrDisplayStatus::Done)
            {
                continue;
            }
            let pr = gh
                .get_pr(&local.target.owner, &local.target.repo, *number)
                .await
                .with_context(|| format!("looking up PR #{number}"))?;
            out.push(Candidate {
                number: pr.number,
                title: pr.title,
                url: pr.html_url,
                head_sha: pr.head_sha,
                rollup: None,
                check_counts: None,
                local: false,
            });
        }
        Ok(out)
    }

    /// Reconcile the watched set with this tick's candidates: add new PRs,
    /// track new heads, and drop PRs that fell out of the selection.
    /// Merged/closed PRs are not dropped; they stay as
    /// [`PrDisplayStatus::Done`] rows so the checkmark keeps marking the win.
    fn sync_watched_set(
        &mut self,
        candidates: &[Candidate],
        open: &[PrWithCiStatus],
        events: &mut Vec<WatchEvent>,
    ) {
        let wanted = candidates.iter().map(|c| c.number).collect::<HashSet<_>>();
        let open_numbers = open.iter().map(|pr| pr.number).collect::<HashSet<_>>();
        let gone = self
            .prs
            .keys()
            .filter(|number| !wanted.contains(number))
            .copied()
            .collect::<Vec<_>>();
        for number in gone {
            let Some(pr) = self.prs.get_mut(&number) else {
                continue;
            };
            if pr.status == PrDisplayStatus::Done {
                continue;
            }
            if pr.local && !open_numbers.contains(&number) {
                pr.status = PrDisplayStatus::Done;
                events.push(WatchEvent::ClosedOrMerged { number });
            } else {
                self.prs.remove(&number);
                events.push(WatchEvent::Deselected { number });
            }
        }
        for candidate in candidates {
            if let Some(pr) = self.prs.get_mut(&candidate.number) {
                if pr.status == PrDisplayStatus::Done {
                    continue;
                }
                // A PR the open-PR search used to return that now only
                // resolves via direct number lookup was merged or closed;
                // retire it before a stale failed run gets retried forever.
                if pr.local && !candidate.local {
                    pr.status = PrDisplayStatus::Done;
                    events.push(WatchEvent::ClosedOrMerged {
                        number: candidate.number,
                    });
                    continue;
                }
                pr.title.clone_from(&candidate.title);
                pr.url.clone_from(&candidate.url);
                pr.rollup = candidate.rollup;
                pr.check_counts = candidate.check_counts;
                pr.local = candidate.local;
                if pr.head_sha != candidate.head_sha {
                    pr.head_sha.clone_from(&candidate.head_sha);
                    pr.runs.clear();
                    pr.retries = 0;
                    pr.force = false;
                    events.push(WatchEvent::NewHead {
                        number: candidate.number,
                    });
                }
            } else {
                events.push(WatchEvent::Added {
                    number: candidate.number,
                    title: candidate.title.clone(),
                    url: candidate.url.clone(),
                });
                self.prs.insert(candidate.number, PrState::new(candidate));
            }
        }
    }

    fn announce_all_green(&mut self, events: &mut Vec<WatchEvent>) {
        let all_green = !self.prs.is_empty()
            && self.prs.values().all(|pr| {
                matches!(
                    pr.status,
                    PrDisplayStatus::Green | PrDisplayStatus::NoChecks | PrDisplayStatus::Done
                )
            });
        if all_green && !self.all_green_announced {
            events.push(WatchEvent::AllGreen);
        }
        self.all_green_announced = all_green;
    }

    /// Mark a PR for a forced retry on the next tick: cancel its in-progress
    /// runs and retry its failed ones, bypassing the retry budget.
    fn force_retry(&mut self, number: u64) -> bool {
        let Some(pr) = self.prs.get_mut(&number) else {
            return false;
        };
        if pr.status == PrDisplayStatus::Done {
            return false;
        }
        pr.force = true;
        true
    }

    /// [`Watcher::force_retry`] by row index in display (number) order.
    fn force_retry_at(&mut self, idx: usize) -> bool {
        let number = self.prs.keys().nth(idx).copied();
        number.is_some_and(|number| self.force_retry(number))
    }

    /// Toggle retry-pausing for a PR by row index in display (number) order.
    /// Paused PRs stay watched but are left alone until unpaused or forced.
    fn toggle_pause_at(&mut self, idx: usize) -> bool {
        let Some(pr) = self
            .prs
            .keys()
            .nth(idx)
            .copied()
            .and_then(|number| self.prs.get_mut(&number))
        else {
            return false;
        };
        if pr.status == PrDisplayStatus::Done {
            return false;
        }
        pr.paused = !pr.paused;
        true
    }
}

/// Whether this tick must fetch the PR's workflow runs. Green and healthy
/// pending PRs are skipped entirely; the rollup from the one search request
/// is enough for them.
fn needs_inspection(pr: &PrState, opts: &WatchOpts) -> bool {
    pr.force
        || pr.rollup.is_none()
        || matches!(pr.rollup, Some(CiStatus::Failed))
        || (opts.mode.cancel && matches!(pr.rollup, Some(CiStatus::Pending)))
        || pr
            .runs
            .values()
            .any(|run| run.restart_grace > 0 || run.cancel_requested)
}

async fn process_pr(
    gh: &impl Gh,
    target: &Target,
    pr: &mut PrState,
    opts: &WatchOpts,
) -> Result<Vec<WatchEvent>> {
    let mut events = Vec::new();
    let was_green = pr.status == PrDisplayStatus::Green;
    // A paused PR is still watched, but the cheap rollup is the only source:
    // no runs are listed, cancelled, or retried. A forced retry (`R`) still
    // acts, bypassing the pause for that tick.
    if pr.paused && !pr.force {
        pr.status = match pr.rollup {
            Some(CiStatus::Pending) => PrDisplayStatus::Pending,
            Some(CiStatus::None) => PrDisplayStatus::NoChecks,
            Some(CiStatus::Failed) => PrDisplayStatus::Failed,
            Some(CiStatus::Success) => PrDisplayStatus::Green,
            // Non-local PRs have no rollup; freeze the last status.
            None => pr.status,
        };
        if pr.status == PrDisplayStatus::Green && !was_green {
            events.push(WatchEvent::Green { number: pr.number });
        }
        return Ok(events);
    }
    if !needs_inspection(pr, opts) {
        pr.status = match pr.rollup {
            Some(CiStatus::Pending) => PrDisplayStatus::Pending,
            Some(CiStatus::None) => PrDisplayStatus::NoChecks,
            // Failed rollups and non-local PRs (`None`) always take the
            // inspection path above.
            Some(CiStatus::Success | CiStatus::Failed) | None => PrDisplayStatus::Green,
        };
        if pr.status == PrDisplayStatus::Green && !was_green {
            events.push(WatchEvent::Green { number: pr.number });
        }
        return Ok(events);
    }
    let runs = gh
        .list_workflow_runs_for_sha(&target.owner, &target.repo, &pr.head_sha)
        .await
        .with_context(|| format!("listing workflow runs for PR #{}", pr.number))?;
    pr.runs.retain(|id, _| runs.iter().any(|run| run.id == *id));
    let (completed, in_progress) = split_completed(runs);
    for run in &in_progress {
        if let Some(state) = pr.runs.get_mut(&run.id) {
            state.restart_grace = 0;
        }
    }
    // A run that settled in a non-retryable state (typically: the rerun
    // succeeded between polls, without the in-progress snapshot ever being
    // observed) is finished business: drop its bookkeeping so a stale
    // restart-grace or cancel flag cannot pin the PR in "retrying" and cost
    // an inspection every tick forever. A later fresh failure starts over
    // with a new budget, just as a new run ID would.
    for run in &completed {
        if !run
            .conclusion
            .is_some_and(crate::gh::WorkflowRunConclusion::is_retryable_failure)
        {
            pr.runs.remove(&run.id);
        }
    }
    cancel_doomed(gh, target, pr, opts, &in_progress, &mut events).await?;
    retry_failed(gh, target, pr, opts, &completed, &mut events).await?;
    pr.force = false;
    pr.status = display_status(pr, &completed, &in_progress);
    if pr.status == PrDisplayStatus::Green && !was_green {
        events.push(WatchEvent::Green { number: pr.number });
    }
    Ok(events)
}

/// Cancel in-progress runs that are already doomed. Unlike the one-shot
/// `--cancel` path this never blocks waiting for the cancellation to settle:
/// the run comes back as completed/cancelled on a later tick, which is a
/// retryable failure, and gets retried then. A forced retry treats every
/// in-progress run as doomed; otherwise only runs that already contain a
/// failed job qualify, even with `--all-jobs`.
async fn cancel_doomed(
    gh: &impl Gh,
    target: &Target,
    pr: &mut PrState,
    opts: &WatchOpts,
    in_progress: &[WorkflowRun],
    events: &mut Vec<WatchEvent>,
) -> Result<()> {
    if !(opts.mode.cancel || pr.force) {
        return Ok(());
    }
    for run in in_progress {
        if pr
            .runs
            .get(&run.id)
            .is_some_and(|state| state.cancel_requested)
        {
            continue;
        }
        let doomed = pr.force || {
            let jobs = gh
                .list_jobs_for_run(&target.owner, &target.repo, run.id)
                .await
                .with_context(|| {
                    format!(
                        "listing jobs for workflow run {} on PR #{}",
                        run.id, pr.number
                    )
                })?;
            first_failed_job(&jobs).is_some()
        };
        if !doomed {
            continue;
        }
        gh.cancel_workflow_run(&target.owner, &target.repo, run.id)
            .await
            .with_context(|| format!("cancelling workflow run {} on PR #{}", run.id, pr.number))?;
        pr.runs.entry(run.id).or_default().cancel_requested = true;
        events.push(WatchEvent::CancelledRun {
            number: pr.number,
            run: run.id,
        });
    }
    Ok(())
}

/// Retry completed runs with a retryable failure, within the per-run budget.
async fn retry_failed(
    gh: &impl Gh,
    target: &Target,
    pr: &mut PrState,
    opts: &WatchOpts,
    completed: &[WorkflowRun],
    events: &mut Vec<WatchEvent>,
) -> Result<()> {
    for run in completed {
        if !run
            .conclusion
            .is_some_and(crate::gh::WorkflowRunConclusion::is_retryable_failure)
        {
            continue;
        }
        let number = pr.number;
        let forced = pr.force;
        let state = pr.runs.entry(run.id).or_default();
        state.cancel_requested = false;
        // The grace window only guards against auto-retrying a stale
        // snapshot; an explicit `R` means "retry right now" and skips it.
        if !forced && state.restart_grace > 0 {
            state.restart_grace -= 1;
            continue;
        }
        if forced {
            // A manual retry overrides the budget and grants a fresh one.
            state.given_up = false;
            state.retries = 0;
        } else if state.given_up {
            continue;
        } else if opts.max_retries.is_some_and(|max| state.retries >= max) {
            state.given_up = true;
            events.push(WatchEvent::GaveUp {
                number,
                run: run.id,
                retries: state.retries,
            });
            continue;
        }
        let result = if opts.mode.all_jobs {
            gh.rerun_workflow_run(&target.owner, &target.repo, run.id)
                .await
        } else {
            gh.rerun_failed_jobs(&target.owner, &target.repo, run.id)
                .await
        };
        result.with_context(|| format!("re-running workflow run {} on PR #{number}", run.id))?;
        state.retries += 1;
        state.restart_grace = RESTART_GRACE_TICKS;
        events.push(WatchEvent::RetriedRun {
            number,
            run: run.id,
            attempt: state.retries,
        });
        pr.retries += 1;
    }
    Ok(())
}

fn display_status(
    pr: &PrState,
    completed: &[WorkflowRun],
    in_progress: &[WorkflowRun],
) -> PrDisplayStatus {
    let failed = completed
        .iter()
        .filter(|run| {
            run.conclusion
                .is_some_and(crate::gh::WorkflowRunConclusion::is_retryable_failure)
        })
        .collect::<Vec<_>>();
    if failed
        .iter()
        .any(|run| pr.runs.get(&run.id).is_some_and(|state| state.given_up))
    {
        return PrDisplayStatus::GivenUp;
    }
    if pr.runs.values().any(|state| state.restart_grace > 0) {
        return PrDisplayStatus::Retrying;
    }
    if !in_progress.is_empty() {
        return PrDisplayStatus::Pending;
    }
    // A failed rollup with no retryable run covers failures jj-gh cannot
    // retry, e.g. a non-Actions commit status.
    if !failed.is_empty() || matches!(pr.rollup, Some(CiStatus::Failed)) {
        return PrDisplayStatus::Failed;
    }
    // A pending rollup with no runs listed yet: checks exist but Actions has
    // not materialized them, so this is not green yet.
    if matches!(pr.rollup, Some(CiStatus::Pending)) {
        return PrDisplayStatus::Pending;
    }
    if !completed.is_empty() || matches!(pr.rollup, Some(CiStatus::Success)) {
        return PrDisplayStatus::Green;
    }
    PrDisplayStatus::NoChecks
}

/// Non-TTY mode: one plain log line per event, no keybinds, runs until
/// killed. Transient poll failures are logged and retried on the next tick;
/// only the first poll is fatal (it surfaces setup errors). When stderr is a
/// terminal, the ambient status line's spinner shows the loop is alive
/// between events.
async fn run_plain(
    model: &impl Model,
    watcher: &mut Watcher,
    remote: &EvalWithCfgFallback<String>,
    upstream_remote: &str,
    interval: Duration,
) -> Result<()> {
    let max_retries = watcher.opts.max_retries;
    let status = crate::ui::status::push("watching PRs");
    let mut first = true;
    loop {
        let result = watcher.tick(model, remote, upstream_remote).await;
        if first {
            log_events(&result?, max_retries);
            first = false;
        } else if let Ok(events) =
            result.inspect_err(|e| log::warn!("watch poll failed (will retry): {e:#}"))
        {
            log_events(&events, max_retries);
        }
        status.set_message(format!(
            "watching {} PR(s); polling every {}s",
            watcher.prs.len(),
            interval.as_secs()
        ));
        tokio::time::sleep(interval).await;
    }
}

fn log_events(events: &[WatchEvent], max_retries: Option<u32>) {
    for event in events {
        match event {
            WatchEvent::Added { number, title, url } => {
                log::info!("watching PR #{number}: {title} <{url}>");
            }
            WatchEvent::ClosedOrMerged { number } => {
                log::info!("PR #{number} was merged or closed ✓ (leaving it listed)");
            }
            WatchEvent::Deselected { number } => {
                log::info!("PR #{number} no longer matches the selection; dropping");
            }
            WatchEvent::NewHead { number } => {
                log::info!("PR #{number}: new push; watching the new head");
            }
            WatchEvent::CancelledRun { number, run } => {
                log::info!("PR #{number}: cancelling doomed run {run}");
            }
            WatchEvent::RetriedRun {
                number,
                run,
                attempt,
            } => {
                let budget = max_retries.map_or(String::new(), |max| format!("/{max}"));
                log::info!("PR #{number}: retrying run {run} (attempt {attempt}{budget})");
            }
            WatchEvent::GaveUp {
                number,
                run,
                retries,
            } => log::warn!(
                "PR #{number}: run {run} still failing after {retries} retries; giving up"
            ),
            WatchEvent::Green { number } => log::info!("PR #{number}: CI is green"),
            WatchEvent::AllGreen => log::info!("all watched PRs are green; still watching"),
            WatchEvent::PrError { number, message } => log::warn!("PR #{number}: {message}"),
        }
    }
}

/// How often the idle footer's "next poll in Ns" countdown refreshes when no
/// row is animating.
const COUNTDOWN_REFRESH: Duration = Duration::from_secs(1);
/// How often spinners advance: the footer one while a poll is in flight, and
/// the row ones while any run is running or retrying.
const SPINNER_REFRESH: Duration = Duration::from_millis(100);

/// The idle redraw cadence: fast while any row shows a spinner, once a second
/// for just the countdown otherwise.
fn idle_refresh(watcher: &Watcher) -> Duration {
    let animating = watcher.prs.values().any(|pr| {
        matches!(
            pr.status,
            PrDisplayStatus::Pending | PrDisplayStatus::Retrying
        )
    });
    if animating {
        SPINNER_REFRESH
    } else {
        COUNTDOWN_REFRESH
    }
}

/// TTY mode: inline list UI, polling between key events. The footer counts
/// down to the next poll and animates a spinner during one, so a quiet watch
/// is visibly not hung.
async fn run_interactive(
    model: &impl Model,
    watcher: &mut Watcher,
    remote: &EvalWithCfgFallback<String>,
    upstream_remote: &str,
    interval: Duration,
) -> Result<()> {
    // The first poll runs before raw mode so setup errors (bad revset, auth)
    // print normally and quit.
    let events = watcher.tick(model, remote, upstream_remote).await?;
    let mut ui = Ui {
        error: error_summary(&events),
        ..Ui::default()
    };
    let mut input = spawn_input_pump();
    loop {
        let deadline = tokio::time::Instant::now() + interval;
        ui.phase = Phase::Idle { deadline };
        ui.render(watcher)?;
        'wait: loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => break 'wait,
                // Full redraws, not footer-only: the row spinners animate too.
                () = tokio::time::sleep(idle_refresh(watcher)) => ui.render(watcher)?,
                key = input.recv() => {
                    let Some(key) = key else { break 'wait };
                    match handle_key(watcher, &mut ui, key) {
                        KeyOutcome::Quit => return Ok(()),
                        KeyOutcome::ForceTick => break 'wait,
                        KeyOutcome::Redraw => ui.render(watcher)?,
                        KeyOutcome::Ignored => {}
                    }
                }
            }
        }
        ui.phase = Phase::Polling;
        ui.render_footer()?;
        // The spinner keeps moving while the poll's API calls run; the row
        // area is left alone until the fresh state is in.
        let result = {
            let tick = watcher.tick(model, remote, upstream_remote);
            let mut tick = std::pin::pin!(tick);
            loop {
                tokio::select! {
                    result = &mut tick => break result,
                    () = tokio::time::sleep(SPINNER_REFRESH) => ui.render_footer()?,
                }
            }
        };
        // Transient poll failures surface in the footer and the loop keeps
        // going; a babysitter should survive network blips.
        ui.error = result.map_or_else(
            |error| Some(format!("{error:#}")),
            |events| error_summary(&events),
        );
    }
}

/// The first per-PR error of the tick, with a count of any further ones.
fn error_summary(events: &[WatchEvent]) -> Option<String> {
    let mut errors = events.iter().filter_map(|event| {
        let WatchEvent::PrError { number, message } = event else {
            return None;
        };
        Some(format!("PR #{number}: {message}"))
    });
    let first = errors.next()?;
    let rest = errors.count();
    Some(if rest > 0 {
        format!("{first} (+{rest} more)")
    } else {
        first
    })
}

/// Reads crossterm key presses on a blocking thread and forwards them to the
/// async loop. Exits shortly after the receiver is dropped.
fn spawn_input_pump() -> tokio::sync::mpsc::Receiver<KeyEvent> {
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    tokio::task::spawn_blocking(move || {
        loop {
            if tx.is_closed() {
                return;
            }
            if !event::poll(Duration::from_millis(200)).unwrap_or(false) {
                continue;
            }
            let Ok(Event::Key(key)) = event::read() else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if tx.blocking_send(key).is_err() {
                return;
            }
        }
    });
    rx
}

enum KeyOutcome {
    Quit,
    /// Poll again right now (a forced retry should not wait out the interval).
    ForceTick,
    Redraw,
    Ignored,
}

fn handle_key(watcher: &mut Watcher, ui: &mut Ui, key: KeyEvent) -> KeyOutcome {
    let len = watcher.prs.len();
    let visible = ui.visible_rows();
    match handle_list_nav(key, &mut ui.state, len, visible) {
        Some(ListNav::Quit) => return KeyOutcome::Quit,
        Some(ListNav::Moved) => return KeyOutcome::Redraw,
        None => {}
    }
    // Capital on purpose: the shift is the confirmation.
    if key.code == KeyCode::Char('R') && watcher.force_retry_at(ui.state.selected) {
        return KeyOutcome::ForceTick;
    }
    if key.code == KeyCode::Char('p') && watcher.toggle_pause_at(ui.state.selected) {
        return KeyOutcome::Redraw;
    }
    KeyOutcome::Ignored
}

const FOOTER_LINES: u16 = 2;
const MAX_VISIBLE_ROWS: usize = 15;
const KEYMAP: &str = "j/k=move  g/G=first/last  p=pause  R=force retry (cancel + rerun)  q=quit";
const EMPTY_PLACEHOLDER: &str = "(no PRs selected yet; waiting)";

/// What the watch loop is doing right now, for the footer's liveness cue.
#[derive(Default)]
enum Phase {
    /// Waiting out the poll interval; the footer counts down to `deadline`.
    Idle { deadline: tokio::time::Instant },
    /// A poll's API calls are in flight; the footer spins.
    #[default]
    Polling,
}

#[derive(Default)]
struct Ui {
    session: Option<InlineSession>,
    state: ListState,
    error: Option<String>,
    phase: Phase,
    spinner: usize,
}

impl Ui {
    fn visible_rows(&self) -> usize {
        self.session.as_ref().map_or(1, |session| {
            usize::from(session.rows().saturating_sub(FOOTER_LINES))
        })
    }

    fn render(&mut self, watcher: &Watcher) -> Result<()> {
        self.spinner = self.spinner.wrapping_add(1);
        let rows = bounded_region_rows(watcher.prs.len().max(1), FOOTER_LINES, MAX_VISIBLE_ROWS);
        if self
            .session
            .as_ref()
            .is_none_or(|session| usize::from(session.rows()) != rows)
        {
            // Drop the old region before claiming a new one so the two don't
            // fight over the same terminal lines.
            self.session = None;
            self.session = Some(InlineSession::enter(rows)?);
        }
        let Some(session) = self.session.as_mut() else {
            return Ok(());
        };
        let mut out = std::io::stdout();
        let cols = usize::from(terminal::size()?.0);
        let height = usize::from(session.rows().saturating_sub(FOOTER_LINES));
        self.state
            .move_to(self.state.selected, watcher.prs.len(), height);
        let prs = watcher.prs.values().collect::<Vec<_>>();
        let spinner = self.spinner;
        render_list_rows(
            &mut out,
            session,
            &ListFrame {
                state: &self.state,
                len: prs.len(),
                cols,
                footer_rows: FOOTER_LINES,
                empty_placeholder: EMPTY_PLACEHOLDER,
            },
            |idx| render_row(prs[idx], cols, spinner),
        )?;
        queue!(out, style::Print(self.footer_line(cols)))?;
        out.flush()?;
        Ok(())
    }

    /// Redraw only the footer line (the cursor rests there between frames),
    /// advancing the spinner while polling. Used for liveness updates so the
    /// row area is not repainted several times a second.
    fn render_footer(&mut self) -> Result<()> {
        if matches!(self.phase, Phase::Polling) {
            self.spinner = self.spinner.wrapping_add(1);
        }
        if self.session.is_none() {
            return Ok(());
        }
        let mut out = std::io::stdout();
        let cols = usize::from(terminal::size()?.0);
        queue!(
            out,
            cursor::MoveToColumn(0),
            terminal::Clear(terminal::ClearType::CurrentLine),
            style::Print(self.footer_line(cols)),
        )?;
        out.flush()?;
        Ok(())
    }

    /// Keymap (or the tick's error) on the left, liveness cue on the right.
    fn footer_line(&self, cols: usize) -> String {
        let liveness = match self.phase {
            Phase::Idle { deadline } => {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
                format!("next poll in {secs}s")
            }
            Phase::Polling => {
                format!(
                    "{} polling…",
                    SPINNER_FRAMES[self.spinner % SPINNER_FRAMES.len()]
                )
            }
        };
        let liveness_width = liveness.chars().count();
        let left_plain = self
            .error
            .as_ref()
            .map_or(KEYMAP.to_string(), |error| format!("! {error}"));
        let left_plain = truncate(&left_plain, cols.saturating_sub(liveness_width + 2));
        let pad = cols.saturating_sub(left_plain.chars().count() + liveness_width);
        let left = if self.error.is_some() {
            left_plain.red().to_string()
        } else {
            left_plain
        };
        format!("{left}{:pad$}{}", "", liveness.dark_grey())
    }
}

/// Status cell width: the widest label ("⠋ retrying") is 10 chars.
const STATUS_COL_WIDTH: usize = 10;
/// Check-count cell width: fits e.g. "● 9 / ✓ 99 / ✗ 9".
const COUNTS_COL_WIDTH: usize = 16;
/// Retry cell width: fits "⏸ 99 retries".
const RETRY_COL_WIDTH: usize = 12;
/// Joins the check-count buckets: `● 2 / ✓ 10 / ✗ 1`.
const COUNTS_SEPARATOR: &str = " / ";

fn render_row(pr: &PrState, cols: usize, spinner: usize) -> String {
    let number = format!("#{:<6}", pr.number);
    let retries = format!("{:<RETRY_COL_WIDTH$}", retry_cell(pr.retries, pr.paused));
    let (counts, counts_width) = counts_cell(pr.check_counts);
    let counts_pad = COUNTS_COL_WIDTH.saturating_sub(counts_width);
    // Plain separator width is 3 (" | ") three times, plus the space between
    // the status and check-count cells.
    let prefix_width = number.chars().count()
        + STATUS_COL_WIDTH
        + COUNTS_COL_WIDTH.max(counts_width)
        + retries.chars().count()
        + 10;
    let available = cols.saturating_sub(prefix_width);
    let sep = format!(" {VERTICAL_SEPARATOR} ").dark_grey().to_string();
    format!(
        "{}{sep}{} {counts}{:counts_pad$}{sep}{}{sep}{}",
        number_cell(pr, &number),
        status_cell(pr.status, spinner),
        "",
        if pr.paused {
            retries.as_str().dark_yellow()
        } else {
            retries.as_str().dark_grey()
        },
        truncate(&pr.title, available)
    )
}

/// The per-check tallies, in the `pr log` icon language: `●` in progress
/// (yellow), `✓` passed (green), `✗` failed (red), slash-separated. Zero
/// buckets are omitted; returns the styled cell plus its visible width (blank
/// for PRs without counts, i.e. non-local direct lookups).
fn counts_cell(counts: Option<CheckCounts>) -> (String, usize) {
    let Some(counts) = counts else {
        return (String::new(), 0);
    };
    // (plain, styled) per nonzero bucket; plain drives the width math.
    let mut segments = Vec::new();
    if counts.in_progress > 0 {
        let text = format!("● {}", counts.in_progress);
        let styled = text.as_str().dark_yellow().to_string();
        segments.push((text, styled));
    }
    if counts.passed > 0 {
        let text = format!("✓ {}", counts.passed);
        let styled = text.as_str().green().to_string();
        segments.push((text, styled));
    }
    if counts.failed > 0 {
        let text = format!("✗ {}", counts.failed);
        let styled = text.as_str().red().to_string();
        segments.push((text, styled));
    }
    let width = segments
        .iter()
        .map(|(plain, _)| plain.chars().count())
        .sum::<usize>()
        + COUNTS_SEPARATOR.len() * segments.len().saturating_sub(1);
    let styled = segments
        .iter()
        .map(|(_, styled)| styled.as_str())
        .collect::<Vec<_>>()
        .join(&COUNTS_SEPARATOR.dark_grey().to_string());
    (styled, width)
}

/// How many automatic retries this head has cost so far; blank until the
/// first one so healthy rows stay quiet. A paused PR gets the pause icon
/// (U+23F8, plain Unicode, no special font needed) in front of the count.
fn retry_cell(retries: u32, paused: bool) -> String {
    let count = match retries {
        0 => String::new(),
        1 => "1 retry".to_string(),
        n => format!("{n} retries"),
    };
    match (paused, count.is_empty()) {
        (false, _) => count,
        (true, true) => "⏸".to_string(),
        (true, false) => format!("⏸ {count}"),
    }
}

/// The padded `#123` cell, hyperlinked to the PR (and underlined as the
/// clickable cue, like [`crate::ui::PrLinks::number`]) when the terminal
/// supports it. The padding stays outside the underline and the link.
fn number_cell(pr: &PrState, padded: &str) -> String {
    let label = format!("#{}", pr.number);
    let padding = &padded[label.len()..];
    if pr.url.is_empty() {
        return format!("{}{padding}", label.as_str().cyan());
    }
    let styled = Stream::Stdout.underline(&label.as_str().cyan().to_string());
    format!("{}{padding}", styled.hyperlink(Stream::Stdout, &pr.url))
}

/// The colored status cell, with an animated spinner on the states that mean
/// CI is in motion (`spinner` picks the frame).
fn status_cell(status: PrDisplayStatus, spinner: usize) -> String {
    let label = status_label(status);
    let text = if matches!(status, PrDisplayStatus::Pending | PrDisplayStatus::Retrying) {
        format!(
            "{} {label:<width$}",
            SPINNER_FRAMES[spinner % SPINNER_FRAMES.len()],
            width = STATUS_COL_WIDTH - 2
        )
    } else {
        format!("{label:<STATUS_COL_WIDTH$}")
    };
    let styled = match status {
        PrDisplayStatus::Pending => text.as_str().dark_yellow(),
        PrDisplayStatus::Failed | PrDisplayStatus::GivenUp => text.as_str().red(),
        PrDisplayStatus::Retrying => text.as_str().cyan(),
        PrDisplayStatus::NoChecks => text.as_str().dark_grey(),
        PrDisplayStatus::Green | PrDisplayStatus::Done => text.as_str().green(),
    };
    styled.to_string()
}

fn status_label(status: PrDisplayStatus) -> &'static str {
    match status {
        PrDisplayStatus::Pending => "running",
        PrDisplayStatus::Failed => "failed",
        PrDisplayStatus::Retrying => "retrying",
        PrDisplayStatus::Green => "green ✓",
        PrDisplayStatus::NoChecks => "no checks",
        PrDisplayStatus::GivenUp => "gave up",
        PrDisplayStatus::Done => "done ✓",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        commands::pr::rerun::test_support::{
            FakeGh, FakeJj, commit, job, local_pr, pr_details, wr,
        },
        gh::{WorkflowRunConclusion, WorkflowRunStatus},
        model::TestModel,
    };
    use crossterm::event::KeyModifiers;

    fn pr_url(number: u64) -> String {
        format!("https://github.com/o/r/pull/{number}")
    }

    fn opts(cancel: bool, all_jobs: bool, max_retries: Option<u32>) -> WatchOpts {
        WatchOpts {
            mode: RerunMode { cancel, all_jobs },
            max_retries,
        }
    }

    fn remote() -> EvalWithCfgFallback<String> {
        EvalWithCfgFallback::new(Some("origin".into()), None)
    }

    async fn tick(watcher: &mut Watcher, jj: &FakeJj, gh: &FakeGh) -> Vec<WatchEvent> {
        watcher
            .tick(&TestModel::without_git(jj, gh), &remote(), "upstream")
            .await
            .expect("tick should succeed")
    }

    fn failed_run(id: u64) -> WorkflowRun {
        wr(
            id,
            WorkflowRunStatus::Completed,
            Some(WorkflowRunConclusion::Failure),
        )
    }

    fn running(id: u64) -> WorkflowRun {
        wr(id, WorkflowRunStatus::InProgress, None)
    }

    #[tokio::test]
    async fn all_selection_watches_green_prs_without_listing_their_runs() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![
                local_pr(1, "green", CiStatus::Success),
                local_pr(2, "red", CiStatus::Failed),
            ])
            .with_runs_for_sha("red", vec![failed_run(7)]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        let events = tick(&mut watcher, &jj, &gh).await;

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.listed_shas, vec!["red"]);
        assert_eq!(calls.rerun_failed, vec![7]);
        assert_eq!(watcher.prs[&1].status, PrDisplayStatus::Green);
        assert_eq!(watcher.prs[&2].status, PrDisplayStatus::Retrying);
        assert!(events.contains(&WatchEvent::RetriedRun {
            number: 2,
            run: 7,
            attempt: 1
        }));
    }

    #[tokio::test]
    async fn stale_failure_is_not_retried_again_until_the_rerun_is_observed() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Failed)])
            .with_runs_for_sha("sha", vec![failed_run(7)]) // tick 1: retry
            .with_runs_for_sha("sha", vec![failed_run(7)]) // tick 2: stale, grace
            .with_runs_for_sha("sha", vec![running(7)]) // tick 3: restart observed
            .with_runs_for_sha("sha", vec![failed_run(7)]); // tick 4: fresh failure
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        for _ in 0..4 {
            tick(&mut watcher, &jj, &gh).await;
        }

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun_failed, vec![7, 7]);
        assert_eq!(watcher.prs[&1].retries, 2);
    }

    #[tokio::test]
    async fn runs_that_exhaust_the_budget_are_given_up() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Failed)])
            .with_runs_for_sha("sha", vec![failed_run(7)]) // tick 1: retry 1/1
            .with_runs_for_sha("sha", vec![running(7)]) // tick 2: restart observed
            .with_runs_for_sha("sha", vec![failed_run(7)]); // tick 3+: budget hit
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, Some(1)));

        let mut events = Vec::new();
        for _ in 0..4 {
            events.append(&mut tick(&mut watcher, &jj, &gh).await);
        }

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun_failed, vec![7]);
        assert_eq!(watcher.prs[&1].status, PrDisplayStatus::GivenUp);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, WatchEvent::GaveUp { .. }))
                .count(),
            1,
            "give-up should be announced exactly once"
        );
    }

    #[tokio::test]
    async fn cancel_mode_cancels_doomed_runs_once_then_retries_after_settling() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Pending)])
            .with_jobs(7, vec![job(100, Some(WorkflowRunConclusion::Failure))])
            .with_runs_for_sha("sha", vec![running(7)]) // tick 1: doomed, cancel
            .with_runs_for_sha("sha", vec![running(7)]) // tick 2: still cancelling
            .with_runs_for_sha(
                "sha",
                vec![wr(
                    7,
                    WorkflowRunStatus::Completed,
                    Some(WorkflowRunConclusion::Cancelled),
                )],
            ); // tick 3: settled, retry
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(true, false, None));

        for _ in 0..3 {
            tick(&mut watcher, &jj, &gh).await;
        }

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.cancelled, vec![7], "cancel must not repeat per tick");
        assert_eq!(calls.list_jobs, vec![7]);
        assert_eq!(calls.rerun_failed, vec![7]);
    }

    #[tokio::test]
    async fn without_cancel_in_progress_runs_are_left_alone() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Failed)])
            .with_jobs(8, vec![job(100, Some(WorkflowRunConclusion::Failure))])
            .with_runs_for_sha("sha", vec![failed_run(7), running(8)]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        tick(&mut watcher, &jj, &gh).await;

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.cancelled, [] as [u64; 0]);
        assert_eq!(calls.list_jobs, [] as [u64; 0]);
        assert_eq!(calls.rerun_failed, vec![7]);
    }

    #[tokio::test]
    async fn new_push_switches_to_the_new_head_and_resets_retry_state() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "a", CiStatus::Failed)])
            .with_local_prs(vec![local_pr(1, "b", CiStatus::Failed)])
            .with_runs_for_sha("a", vec![failed_run(5)])
            .with_runs_for_sha("b", vec![failed_run(6)]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        tick(&mut watcher, &jj, &gh).await;
        let events = tick(&mut watcher, &jj, &gh).await;

        assert!(events.contains(&WatchEvent::NewHead { number: 1 }));
        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.listed_shas, vec!["a", "b"]);
        assert_eq!(calls.rerun_failed, vec![5, 6]);
        assert_eq!(watcher.prs[&1].retries, 1, "retry count resets on new head");
    }

    #[tokio::test]
    async fn merged_prs_stay_listed_with_a_checkmark() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Success)])
            .with_local_prs(vec![]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        tick(&mut watcher, &jj, &gh).await;
        let events = tick(&mut watcher, &jj, &gh).await;
        assert!(events.contains(&WatchEvent::ClosedOrMerged { number: 1 }));
        assert_eq!(watcher.prs[&1].status, PrDisplayStatus::Done);

        let events = tick(&mut watcher, &jj, &gh).await;
        assert_eq!(
            watcher.prs[&1].status,
            PrDisplayStatus::Done,
            "the checkmark row must stay listed"
        );
        assert!(
            !events.contains(&WatchEvent::ClosedOrMerged { number: 1 }),
            "the farewell must not repeat"
        );
        // Done rows are retired: no API calls are spent on them.
        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.listed_shas, [] as [String; 0]);
    }

    #[tokio::test]
    async fn new_prs_are_picked_up_while_watching() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha1", CiStatus::Success)])
            .with_local_prs(vec![
                local_pr(1, "sha1", CiStatus::Success),
                local_pr(2, "sha2", CiStatus::Success),
            ]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        tick(&mut watcher, &jj, &gh).await;
        let events = tick(&mut watcher, &jj, &gh).await;

        assert!(events.contains(&WatchEvent::Added {
            number: 2,
            title: "t".into(),
            url: pr_url(2),
        }));
        assert_eq!(watcher.prs.len(), 2);
    }

    #[tokio::test]
    async fn revset_selection_filters_and_picks_up_new_matches() {
        let mut other = local_pr(2, "sha2", CiStatus::Success);
        other.head_ref_name = "other".into();
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha1", CiStatus::Success), other]);
        let jj = FakeJj::with_revset_commits(vec![commit("aaa", &["feat"])]);
        let mut watcher = Watcher::new(
            Selection::Explicit {
                numbers: vec![],
                revsets: vec!["mine()".into()],
            },
            opts(false, false, None),
        );

        tick(&mut watcher, &jj, &gh).await;
        assert_eq!(watcher.prs.keys().copied().collect::<Vec<_>>(), vec![1]);

        jj.set_revset_commits(vec![commit("aaa", &["feat"]), commit("bbb", &["other"])]);
        let events = tick(&mut watcher, &jj, &gh).await;
        assert!(events.contains(&WatchEvent::Added {
            number: 2,
            title: "t".into(),
            url: pr_url(2),
        }));
        assert_eq!(watcher.prs.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
    }

    #[tokio::test]
    async fn prs_leaving_the_revset_are_deselected_immediately() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![]).with_local_prs(vec![local_pr(
            1,
            "sha1",
            CiStatus::Success,
        )]);
        let jj = FakeJj::with_revset_commits(vec![commit("aaa", &["feat"])]);
        let mut watcher = Watcher::new(
            Selection::Explicit {
                numbers: vec![],
                revsets: vec!["mine()".into()],
            },
            opts(false, false, None),
        );

        tick(&mut watcher, &jj, &gh).await;
        jj.set_revset_commits(vec![]);
        let events = tick(&mut watcher, &jj, &gh).await;

        assert!(events.contains(&WatchEvent::Deselected { number: 1 }));
        assert!(watcher.prs.is_empty());
    }

    #[tokio::test]
    async fn explicit_numbers_without_a_local_pr_fall_back_to_direct_lookup() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_pr(pr_details(99, "sha99"))
            .with_runs_for_sha("sha99", vec![failed_run(7)]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(
            Selection::Explicit {
                numbers: vec![99],
                revsets: vec![],
            },
            opts(false, false, None),
        );

        let events = tick(&mut watcher, &jj, &gh).await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, WatchEvent::Added { number: 99, .. }))
        );
        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun_failed, vec![7]);
        assert_eq!(watcher.prs[&99].status, PrDisplayStatus::Retrying);
    }

    #[tokio::test]
    async fn forced_retry_cancels_in_progress_runs_and_bypasses_the_budget() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Failed)])
            .with_jobs(8, vec![job(100, Some(WorkflowRunConclusion::Success))])
            .with_runs_for_sha("sha", vec![failed_run(7), running(8)]);
        let jj = FakeJj::default();
        // Budget of 0: run 7 is given up on the very first tick.
        let mut watcher = Watcher::new(Selection::All, opts(false, false, Some(0)));

        let events = tick(&mut watcher, &jj, &gh).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, WatchEvent::GaveUp { .. }))
        );
        assert_eq!(watcher.prs[&1].status, PrDisplayStatus::GivenUp);

        assert!(watcher.force_retry(1));
        let events = tick(&mut watcher, &jj, &gh).await;

        let calls = gh.calls.lock().unwrap();
        assert_eq!(
            calls.cancelled,
            vec![8],
            "force treats healthy runs as doomed"
        );
        assert_eq!(calls.rerun_failed, vec![7]);
        assert!(events.contains(&WatchEvent::RetriedRun {
            number: 1,
            run: 7,
            attempt: 1
        }));
    }

    #[tokio::test]
    async fn all_green_is_announced_once() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![]).with_local_prs(vec![local_pr(
            1,
            "sha",
            CiStatus::Success,
        )]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        let events = tick(&mut watcher, &jj, &gh).await;
        assert!(events.contains(&WatchEvent::AllGreen));

        let events = tick(&mut watcher, &jj, &gh).await;
        assert!(!events.contains(&WatchEvent::AllGreen));
    }

    #[tokio::test]
    async fn per_pr_failures_do_not_stop_other_prs() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![
                local_pr(1, "bad", CiStatus::Failed),
                local_pr(2, "good", CiStatus::Failed),
            ])
            .with_list_failure("bad")
            .with_runs_for_sha("good", vec![failed_run(7)]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        let events = tick(&mut watcher, &jj, &gh).await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, WatchEvent::PrError { number: 1, .. }))
        );
        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun_failed, vec![7]);
    }

    #[tokio::test]
    async fn all_jobs_uses_full_rerun_for_retries() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Failed)])
            .with_runs_for_sha("sha", vec![failed_run(7)]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, true, None));

        tick(&mut watcher, &jj, &gh).await;

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun, vec![7]);
        assert_eq!(calls.rerun_failed, [] as [u64; 0]);
    }

    #[test]
    fn selection_defaults_to_all_and_splits_numbers_from_revsets() {
        assert!(matches!(Selection::from_args(&[], false), Selection::All));
        assert!(matches!(Selection::from_args(&[], true), Selection::All));
        let Selection::Explicit { numbers, revsets } =
            Selection::from_args(&["42".into(), "mine()".into()], false)
        else {
            panic!("explicit args should produce an explicit selection");
        };
        assert_eq!(numbers, vec![42]);
        assert_eq!(revsets, vec!["mine()".to_string()]);
    }

    #[test]
    fn keys_map_to_quit_force_and_movement() {
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));
        let mut ui = Ui::default();
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);

        assert!(matches!(
            handle_key(&mut watcher, &mut ui, press(KeyCode::Char('q'))),
            KeyOutcome::Quit
        ));
        assert!(matches!(
            handle_key(
                &mut watcher,
                &mut ui,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            KeyOutcome::Quit
        ));
        // R on an empty list is a no-op, not a forced poll.
        assert!(matches!(
            handle_key(&mut watcher, &mut ui, press(KeyCode::Char('R'))),
            KeyOutcome::Ignored
        ));
        assert!(matches!(
            handle_key(&mut watcher, &mut ui, press(KeyCode::Char('j'))),
            KeyOutcome::Redraw
        ));
    }

    #[tokio::test]
    async fn retried_run_that_succeeds_between_polls_returns_to_green() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Failed)])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Success)])
            .with_runs_for_sha("sha", vec![failed_run(7)]) // tick 1: retry
            .with_runs_for_sha(
                "sha",
                vec![wr(
                    7,
                    WorkflowRunStatus::Completed,
                    Some(WorkflowRunConclusion::Success),
                )],
            ); // tick 2: rerun finished green, in-progress snapshot never seen
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        tick(&mut watcher, &jj, &gh).await;
        let events = tick(&mut watcher, &jj, &gh).await;

        assert_eq!(watcher.prs[&1].status, PrDisplayStatus::Green);
        assert!(events.contains(&WatchEvent::Green { number: 1 }));

        // The stale restart grace is gone: the green PR stops costing a
        // runs-listing call per tick.
        tick(&mut watcher, &jj, &gh).await;
        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.listed_shas, vec!["sha", "sha"]);
    }

    #[tokio::test]
    async fn cancel_request_state_is_dropped_when_the_run_settles_green() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Pending)])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Success)])
            .with_jobs(7, vec![job(100, Some(WorkflowRunConclusion::Failure))])
            .with_runs_for_sha("sha", vec![running(7)]) // tick 1: doomed, cancelled
            .with_runs_for_sha(
                "sha",
                vec![wr(
                    7,
                    WorkflowRunStatus::Completed,
                    Some(WorkflowRunConclusion::Success),
                )],
            ); // tick 2: finished green before the cancel landed
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(true, false, None));

        tick(&mut watcher, &jj, &gh).await;
        tick(&mut watcher, &jj, &gh).await;
        assert_eq!(watcher.prs[&1].status, PrDisplayStatus::Green);

        tick(&mut watcher, &jj, &gh).await;
        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.cancelled, vec![7]);
        assert_eq!(
            calls.listed_shas,
            vec!["sha", "sha"],
            "a stale cancel flag must not keep forcing inspections"
        );
    }

    #[tokio::test]
    async fn explicit_number_pr_that_merges_is_retired_with_a_checkmark() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_pr(pr_details(42, "sha"))
            .with_local_prs(vec![local_pr(42, "sha", CiStatus::Success)])
            .with_local_prs(vec![]) // merged: gone from the open-PR search
            .with_runs_for_sha("sha", vec![failed_run(7)]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(
            Selection::Explicit {
                numbers: vec![42],
                revsets: vec![],
            },
            opts(false, false, None),
        );

        tick(&mut watcher, &jj, &gh).await;
        let events = tick(&mut watcher, &jj, &gh).await;

        assert!(events.contains(&WatchEvent::ClosedOrMerged { number: 42 }));
        assert_eq!(watcher.prs[&42].status, PrDisplayStatus::Done);

        // Retired for good: the stale failed run on the merged head is never
        // fetched, let alone retried.
        tick(&mut watcher, &jj, &gh).await;
        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.listed_shas, [] as [String; 0]);
        assert_eq!(calls.rerun_failed, [] as [u64; 0]);
    }

    #[tokio::test]
    async fn prs_without_checks_are_not_reported_green() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![]).with_local_prs(vec![local_pr(
            1,
            "sha",
            CiStatus::None,
        )]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        let events = tick(&mut watcher, &jj, &gh).await;

        assert_eq!(watcher.prs[&1].status, PrDisplayStatus::NoChecks);
        assert!(!events.iter().any(|e| matches!(e, WatchEvent::Green { .. })));
        // Nothing can fail, so the all-green announcement is not blocked, and
        // no runs are fetched for it.
        assert!(events.contains(&WatchEvent::AllGreen));
        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.listed_shas, [] as [String; 0]);
    }

    #[test]
    fn status_cells_are_constant_width_and_retry_cell_hides_zero() {
        assert_eq!(retry_cell(0, false), "");
        assert_eq!(retry_cell(1, false), "1 retry");
        assert_eq!(retry_cell(3, false), "3 retries");
        assert_eq!(retry_cell(0, true), "⏸");
        assert_eq!(retry_cell(3, true), "⏸ 3 retries");
        for status in [
            PrDisplayStatus::Pending,
            PrDisplayStatus::Failed,
            PrDisplayStatus::Retrying,
            PrDisplayStatus::Green,
            PrDisplayStatus::NoChecks,
            PrDisplayStatus::GivenUp,
            PrDisplayStatus::Done,
        ] {
            let (_, visible) =
                crate::ui::tui::apply_bg_highlight(&status_cell(status, 0), "\u{1b}[48;5;236m");
            assert_eq!(visible, STATUS_COL_WIDTH, "width drifted for {status:?}");
        }
    }

    #[test]
    fn counts_cell_uses_the_pr_log_icon_language_and_skips_zero_buckets() {
        assert_eq!(counts_cell(None), (String::new(), 0));
        assert_eq!(
            counts_cell(Some(CheckCounts::default())),
            (String::new(), 0)
        );

        let (styled, width) = counts_cell(Some(CheckCounts {
            in_progress: 2,
            passed: 10,
            failed: 1,
        }));
        assert_eq!(width, "● 2 / ✓ 10 / ✗ 1".chars().count());
        let (_, visible) = crate::ui::tui::apply_bg_highlight(&styled, "\u{1b}[48;5;236m");
        assert_eq!(visible, width, "declared width must match visible width");
        assert!(styled.contains("● 2") && styled.contains("✓ 10") && styled.contains("✗ 1"));

        let (styled, width) = counts_cell(Some(CheckCounts {
            in_progress: 0,
            passed: 7,
            failed: 0,
        }));
        assert_eq!(width, 3);
        assert!(styled.contains("✓ 7") && !styled.contains('●') && !styled.contains('✗'));
    }

    #[tokio::test]
    async fn check_counts_flow_from_the_search_to_the_watched_state() {
        let mut pr = local_pr(1, "sha", CiStatus::Pending);
        pr.check_counts = CheckCounts {
            in_progress: 3,
            passed: 1,
            failed: 0,
        };
        let gh = FakeGh::new(pr_details(0, "unused"), vec![]).with_local_prs(vec![pr]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        tick(&mut watcher, &jj, &gh).await;

        assert_eq!(
            watcher.prs[&1].check_counts,
            Some(CheckCounts {
                in_progress: 3,
                passed: 1,
                failed: 0,
            })
        );
    }

    #[tokio::test]
    async fn paused_prs_are_watched_but_not_retried_until_unpaused() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Failed)])
            .with_runs_for_sha("sha", vec![failed_run(7)]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        tick(&mut watcher, &jj, &gh).await;
        assert!(watcher.toggle_pause_at(0), "pause the only row");
        tick(&mut watcher, &jj, &gh).await;

        {
            let calls = gh.calls.lock().unwrap();
            assert_eq!(
                calls.listed_shas,
                vec!["sha"],
                "a paused PR must not even list runs"
            );
            assert_eq!(calls.rerun_failed, vec![7], "only the pre-pause retry");
        }
        assert_eq!(
            watcher.prs[&1].status,
            PrDisplayStatus::Failed,
            "paused status comes from the rollup"
        );

        assert!(watcher.toggle_pause_at(0), "unpause");
        // Unpaused again: the still-failing run is acted on (the stale grace
        // from the pre-pause retry is consumed first).
        tick(&mut watcher, &jj, &gh).await;
        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.listed_shas, vec!["sha", "sha"]);
    }

    #[tokio::test]
    async fn forced_retry_overrides_a_pause_for_that_tick() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![])
            .with_local_prs(vec![local_pr(1, "sha", CiStatus::Failed)])
            .with_runs_for_sha("sha", vec![failed_run(7)]);
        let jj = FakeJj::default();
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));

        tick(&mut watcher, &jj, &gh).await;
        assert!(watcher.toggle_pause_at(0));
        assert!(watcher.force_retry(1));
        tick(&mut watcher, &jj, &gh).await;

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun_failed, vec![7, 7], "R acts despite the pause");
        assert!(watcher.prs[&1].paused, "the pause itself is sticky");
    }

    #[test]
    fn p_toggles_pause_on_the_selected_row() {
        let mut watcher = Watcher::new(Selection::All, opts(false, false, None));
        let mut ui = Ui::default();
        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);

        // p on an empty list is a no-op.
        assert!(matches!(
            handle_key(&mut watcher, &mut ui, press(KeyCode::Char('p'))),
            KeyOutcome::Ignored
        ));
    }

    #[test]
    fn error_summary_reports_the_first_error_and_counts_the_rest() {
        let events = vec![
            WatchEvent::PrError {
                number: 1,
                message: "boom".into(),
            },
            WatchEvent::PrError {
                number: 2,
                message: "bang".into(),
            },
        ];
        assert_eq!(error_summary(&events).unwrap(), "PR #1: boom (+1 more)");
        assert_eq!(error_summary(&[]), None);
    }
}
