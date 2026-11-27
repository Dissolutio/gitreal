use crate::model::{JobAction, JobFinished, JobId, JobStarted};
use crate::repo::reader::EventSink;
use std::{
    ffi::OsString,
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub const JOB_TIMEOUT: Duration = Duration::from_secs(5 * 60);
static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

pub struct JobSpec {
    pub kind: String,
    pub label: String,
    pub cwd: PathBuf,
    pub args: Vec<String>,
    pub additional_steps: Vec<Vec<String>>,
    pub rollback_steps: Vec<Vec<String>>,
    pub step_labels: Vec<String>,
    pub preconditions: Vec<JobPrecondition>,
    pub action_on_failure: Option<JobAction>,
    pub envs: Vec<(OsString, OsString)>,
    pub success_message: String,
    pub timeout: Duration,
    program: OsString,
}

pub enum JobPrecondition {
    BranchOid { branch: String, expected: String },
    CurrentBranch { branch: String },
    WorkingTreeClean,
    HeadOid { expected: String },
    NoOperation,
    OperationIs { kind: String },
    StashOid { index: usize, expected: String },
}

impl JobSpec {
    pub fn new(
        kind: impl Into<String>,
        label: impl Into<String>,
        cwd: PathBuf,
        args: Vec<String>,
        success_message: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            label: label.into(),
            cwd,
            args,
            additional_steps: Vec::new(),
            rollback_steps: Vec::new(),
            step_labels: Vec::new(),
            preconditions: Vec::new(),
            action_on_failure: None,
            envs: Vec::new(),
            success_message: success_message.into(),
            timeout: JOB_TIMEOUT,
            program: OsString::from("git"),
        }
    }
}

enum QueueMessage {
    Run {
        id: JobId,
        spec: JobSpec,
        after_job: Box<dyn FnOnce() + Send>,
    },
}

pub struct JobQueue {
    sender: mpsc::Sender<QueueMessage>,
    cancelled: Arc<AtomicBool>,
}

impl JobQueue {
    pub fn new(repo_id: String, event_sink: Arc<dyn EventSink>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_repo_id = repo_id.clone();
        thread::Builder::new()
            .name(format!("git-jobs-{repo_id}"))
            .spawn(move || {
                while let Ok(QueueMessage::Run {
                    id,
                    spec,
                    after_job,
                }) = receiver.recv()
                {
                    if worker_cancelled.load(Ordering::Acquire) {
                        event_sink.job_finished(JobFinished {
                            job_id: id,
                            repo_id: worker_repo_id.clone(),
                            kind: spec.kind,
                            label: spec.label,
                            ok: false,
                            message: "Job cancelled because the repository was closed".into(),
                            stderr_tail: String::new(),
                            action: None,
                        });
                        continue;
                    }

                    event_sink.job_started(JobStarted {
                        job_id: id.clone(),
                        repo_id: worker_repo_id.clone(),
                        kind: spec.kind.clone(),
                        label: spec.label.clone(),
                    });
                    let result = run_git(&spec);
                    after_job();
                    let (ok, message, stderr_tail, action) = match result {
                        Ok(output) if output.success => {
                            let _captured_output = (&output.stdout, output.exit_code);
                            (true, spec.success_message, output.stderr_tail, None)
                        }
                        Ok(output) => {
                            let _captured_output = (&output.stdout, output.exit_code);
                            let conflicts = if is_history_operation(&spec.kind) {
                                conflicted_file_count(&spec)
                            } else {
                                0
                            };
                            if conflicts > 0 {
                                let operation =
                                    match crate::repo::history_ops::detect_operation(&spec.cwd)
                                        .ok()
                                        .flatten()
                                    {
                                        Some("rebase") => "Rebase",
                                        Some("merge") => "Merge",
                                        Some("cherry-pick") => "Cherry-pick",
                                        Some("revert") => "Revert",
                                        _ => "Git operation",
                                    };
                                let noun = if conflicts == 1 {
                                    "conflicted file"
                                } else {
                                    "conflicted files"
                                };
                                let action = JobAction {
                                    kind: "open_in_vscode".into(),
                                    label: "Open in VSCode".into(),
                                    payload: serde_json::json!({}),
                                };
                                (
                                    false,
                                    format!("{operation} stopped: {conflicts} {noun}"),
                                    output.stderr_tail,
                                    Some(action),
                                )
                            } else {
                                let step = spec
                                    .step_labels
                                    .get(output.failed_step)
                                    .cloned()
                                    .unwrap_or_else(|| spec.label.clone());
                                let message =
                                    friendly_failure(&spec, &output.stderr, output.timed_out);
                                let message = if spec.step_labels.len() > 1 {
                                    format!("{step} failed: {message}")
                                } else {
                                    message
                                };
                                let action = spec.action_on_failure.clone().filter(|action| {
                                    (action.kind == "stash_and_retry"
                                        && is_local_changes_failure(&output.stderr))
                                        || (action.kind == "force_delete"
                                            && output
                                                .stderr
                                                .to_ascii_lowercase()
                                                .contains("not fully merged"))
                                });
                                (false, message, output.stderr_tail, action)
                            }
                        }
                        Err(error) => (false, error, String::new(), None),
                    };
                    let message = redact_credentials(&message);
                    let stderr_tail = redact_credentials(&stderr_tail);
                    event_sink.job_finished(JobFinished {
                        job_id: id,
                        repo_id: worker_repo_id.clone(),
                        kind: spec.kind,
                        label: spec.label,
                        ok,
                        message,
                        stderr_tail,
                        action,
                    });
                }
            })
            .expect("failed to start Git job worker");
        Self { sender, cancelled }
    }

    pub fn enqueue(
        &self,
        spec: JobSpec,
        after_job: impl FnOnce() + Send + 'static,
    ) -> std::io::Result<JobId> {
        let id = format!("job-{}", NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed));
        self.sender
            .send(QueueMessage::Run {
                id: id.clone(),
                spec,
                after_job: Box::new(after_job),
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(id)
    }

    pub fn cancel_queued(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct GitOutput {
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    stderr_tail: String,
    timed_out: bool,
    failed_step: usize,
}

fn run_git(spec: &JobSpec) -> Result<GitOutput, String> {
    for precondition in &spec.preconditions {
        let metadata_failure = match precondition {
            JobPrecondition::NoOperation => crate::repo::history_ops::detect_operation(&spec.cwd)
                .map_err(|error| error.message)?
                .map(|kind| format!("{kind} is already in progress")),
            JobPrecondition::OperationIs { kind } => {
                let actual = crate::repo::history_ops::detect_operation(&spec.cwd)
                    .map_err(|error| error.message)?;
                (actual != Some(kind.as_str()))
                    .then(|| "The Git operation is no longer in progress".to_owned())
            }
            _ => None,
        };
        if let Some(message) = metadata_failure {
            return Ok(precondition_failure(message, None));
        }
        let (args, expected) = match precondition {
            JobPrecondition::BranchOid { branch, expected } => (
                vec!["rev-parse".into(), format!("refs/heads/{branch}")],
                Some(expected.clone()),
            ),
            JobPrecondition::CurrentBranch { branch } => (
                vec!["symbolic-ref".into(), "--quiet".into(), "HEAD".into()],
                Some(format!("refs/heads/{branch}")),
            ),
            JobPrecondition::WorkingTreeClean => (
                vec!["status".into(), "--porcelain".into()],
                Some(String::new()),
            ),
            JobPrecondition::HeadOid { expected } => (
                vec!["rev-parse".into(), "HEAD".into()],
                Some(expected.clone()),
            ),
            JobPrecondition::StashOid { index, expected } => (
                vec!["rev-parse".into(), format!("stash@{{{index}}}")],
                Some(expected.clone()),
            ),
            JobPrecondition::NoOperation | JobPrecondition::OperationIs { .. } => continue,
        };
        let result = run_git_args(&spec.program, &spec.cwd, &args, spec.timeout, &spec.envs)?;
        let actual = if matches!(precondition, JobPrecondition::WorkingTreeClean) {
            result.stdout.trim_end()
        } else {
            result.stdout.trim()
        };
        if !result.success || expected.as_deref() != Some(actual) {
            let message = match precondition {
                JobPrecondition::BranchOid { .. } => {
                    "Local branch changed since the operation was requested".into()
                }
                JobPrecondition::CurrentBranch { .. } => {
                    "The checked-out branch changed since the operation was requested".into()
                }
                JobPrecondition::WorkingTreeClean => "The working tree is not clean".into(),
                JobPrecondition::HeadOid { .. } => {
                    "HEAD changed since the operation was requested".into()
                }
                JobPrecondition::StashOid { .. } => "Stash changed since it was selected".into(),
                JobPrecondition::NoOperation | JobPrecondition::OperationIs { .. } => {
                    unreachable!()
                }
            };
            return Ok(precondition_failure(message, result.exit_code));
        }
    }

    let mut steps = Vec::with_capacity(spec.additional_steps.len() + 1);
    steps.push(spec.args.clone());
    steps.extend(spec.additional_steps.iter().cloned());
    let mut last_output = None;
    for (index, args) in steps.iter().enumerate() {
        let output = run_git_args(&spec.program, &spec.cwd, args, spec.timeout, &spec.envs)?;
        if !output.success {
            let mut output = output;
            if index > 0 {
                for rollback_args in &spec.rollback_steps {
                    match run_git_args(
                        &spec.program,
                        &spec.cwd,
                        rollback_args,
                        spec.timeout,
                        &spec.envs,
                    ) {
                        Ok(rollback) if rollback.success => {}
                        Ok(rollback) => {
                            let detail = rollback
                                .stderr
                                .lines()
                                .rev()
                                .find(|line| !line.trim().is_empty())
                                .unwrap_or("unknown error");
                            output.stderr.push_str(&format!(
                                "\nRollback failed; the remote branch may still be deleted: {detail}"
                            ));
                            output.stderr_tail = output
                                .stderr
                                .lines()
                                .rev()
                                .take(20)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect::<Vec<_>>()
                                .join("\n");
                            break;
                        }
                        Err(error) => {
                            output.stderr.push_str(&format!(
                                "\nRollback failed; the remote branch may still be deleted: {error}"
                            ));
                            output.stderr_tail = output
                                .stderr
                                .lines()
                                .rev()
                                .take(20)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect::<Vec<_>>()
                                .join("\n");
                            break;
                        }
                    }
                }
            }
            return Ok(GitOutput {
                failed_step: index,
                ..output
            });
        }
        last_output = Some(output);
    }
    Ok(last_output.unwrap_or(GitOutput {
        success: true,
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        stderr_tail: String::new(),
        timed_out: false,
        failed_step: 0,
    }))
}

fn precondition_failure(message: String, exit_code: Option<i32>) -> GitOutput {
    GitOutput {
        success: false,
        exit_code,
        stderr: message,
        stderr_tail: String::new(),
        stdout: String::new(),
        timed_out: false,
        failed_step: 0,
    }
}

fn is_history_operation(kind: &str) -> bool {
    matches!(
        kind,
        "merge" | "rebase" | "cherry-pick" | "revert" | "continue-operation" | "skip-operation"
    )
}

fn conflicted_file_count(spec: &JobSpec) -> usize {
    let args = ["diff", "--name-only", "--diff-filter=U", "-z"].map(str::to_owned);
    run_git_args(&spec.program, &spec.cwd, &args, spec.timeout, &spec.envs)
        .ok()
        .filter(|output| output.success)
        .map(|output| {
            output
                .stdout
                .as_bytes()
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .count()
        })
        .unwrap_or(0)
}

fn run_git_args(
    program: &OsString,
    cwd: &PathBuf,
    args: &[String],
    timeout: Duration,
    envs: &[(OsString, OsString)],
) -> Result<GitOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_ASKPASS")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.envs(envs.iter().cloned());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start Git: {error}"))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Could not capture Git output".to_owned())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Could not capture Git errors".to_owned())?;
    let (output_sender, output_receiver) = mpsc::channel();
    let stdout_sender = output_sender.clone();
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = stdout_sender.send((true, bytes));
    });
    let stderr_sender = output_sender.clone();
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        let _ = stderr_sender.send((false, bytes));
    });
    drop(output_sender);

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                timed_out = true;
                terminate_child(&mut child);
                break child
                    .wait()
                    .map_err(|error| format!("Could not stop Git: {error}"))?;
            }
            Err(error) => {
                terminate_child(&mut child);
                return Err(format!("Could not wait for Git: {error}"));
            }
        }
    };

    let mut captured_stdout = None;
    let mut captured_stderr = None;
    let output_deadline = if timed_out {
        Instant::now() + Duration::from_millis(250)
    } else {
        deadline
    };
    while captured_stdout.is_none() || captured_stderr.is_none() {
        let remaining = output_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            timed_out = true;
            terminate_child(&mut child);
            break;
        }
        match output_receiver.recv_timeout(remaining) {
            Ok((true, output)) => captured_stdout = Some(output),
            Ok((false, output)) => captured_stderr = Some(output),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                terminate_child(&mut child);
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if captured_stdout.is_some() && captured_stderr.is_some() {
        stdout_reader
            .join()
            .map_err(|_| "Git output reader stopped unexpectedly".to_owned())?;
        stderr_reader
            .join()
            .map_err(|_| "Git error reader stopped unexpectedly".to_owned())?;
    }
    let captured_stdout = captured_stdout.unwrap_or_default();
    let stderr = captured_stderr.unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    let stderr_tail = stderr
        .lines()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    Ok(GitOutput {
        success: status.success() && !timed_out,
        exit_code: status.code(),
        stdout: String::from_utf8_lossy(&captured_stdout).into_owned(),
        stderr,
        stderr_tail,
        timed_out,
        failed_step: 0,
    })
}

fn is_local_changes_failure(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    text.contains("local changes") || text.contains("would be overwritten")
}

fn terminate_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        let _ = Command::new("kill")
            .args(["-KILL", "--", &process_group])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn redact_credentials(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut redacted = String::with_capacity(text.len());
    let mut cursor = 0;

    while cursor < bytes.len() {
        let start = cursor;
        if bytes[cursor].is_ascii_alphabetic() {
            let mut scheme_end = cursor + 1;
            while scheme_end < bytes.len()
                && (bytes[scheme_end].is_ascii_alphanumeric()
                    || matches!(bytes[scheme_end], b'+' | b'.' | b'-'))
            {
                scheme_end += 1;
            }
            if bytes.get(scheme_end..scheme_end + 3) == Some(b"://") {
                let authority_start = scheme_end + 3;
                let mut authority_end = authority_start;
                while authority_end < bytes.len()
                    && !matches!(
                        bytes[authority_end],
                        b'/' | b'?'
                            | b'#'
                            | b' '
                            | b'\t'
                            | b'\n'
                            | b'\r'
                            | b'"'
                            | b'\''
                            | b'<'
                            | b'>'
                    )
                {
                    authority_end += 1;
                }
                if let Some(at) = text[authority_start..authority_end].rfind('@') {
                    let host_start = authority_start + at + 1;
                    if host_start < authority_end {
                        redacted.push_str(&text[start..authority_start]);
                        redacted.push_str("***@");
                        redacted.push_str(&text[host_start..authority_end]);
                        cursor = authority_end;
                        continue;
                    }
                }
            }
        }
        let character = text[cursor..]
            .chars()
            .next()
            .expect("cursor is within text");
        redacted.push(character);
        cursor += character.len_utf8();
    }

    redacted
}

fn friendly_failure(spec: &JobSpec, stderr: &str, timed_out: bool) -> String {
    if timed_out {
        return "Git operation timed out".into();
    }
    let text = stderr.to_ascii_lowercase();
    if matches!(
        spec.kind.as_str(),
        "checkout" | "checkout-remote" | "checkout-commit"
    ) && is_local_changes_failure(stderr)
    {
        "Checkout blocked by local changes; stash them to continue".into()
    } else if text.contains("authentication failed")
        || text.contains("could not read username")
        || text.contains("permission denied (publickey)")
        || text.contains("invalid username or password")
    {
        "Authentication failed; check your Git credentials".into()
    } else if spec.kind == "pull"
        && (text.contains("not possible to fast-forward")
            || text.contains("divergent branches")
            || text.contains("have diverged"))
    {
        "Branch has diverged from upstream; merge or rebase instead".into()
    } else if spec.kind == "force-push"
        && (text.contains("stale info") || text.contains("force-with-lease"))
    {
        "Force push rejected: remote changed since Gitreal last read it".into()
    } else if text.contains("non-fast-forward")
        || text.contains("[rejected]")
        || text.contains("fetch first")
    {
        "Push rejected: remote has new commits".into()
    } else if text.contains("no upstream")
        || text.contains("no tracking information")
        || text.contains("has no upstream branch")
    {
        "No upstream configured for this branch".into()
    } else if text.contains("network is unreachable")
        || text.contains("could not resolve host")
        || text.contains("failed to connect")
        || text.contains("connection refused")
    {
        "Network unreachable; check your connection and remote".into()
    } else if spec
        .preconditions
        .iter()
        .any(|condition| matches!(condition, JobPrecondition::WorkingTreeClean))
        && text.contains("working tree is not clean")
    {
        if spec.kind == "rebase" {
            "Rebase refused: the working tree is not clean; stash changes first".into()
        } else {
            "Reset refused: the working tree is not clean".into()
        }
    } else {
        stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Git operation failed")
            .trim()
            .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{JobFinished, JobStarted, RepoChanged, RepoError};
    use std::sync::mpsc::{self, Receiver};

    enum RecordedJobEvent {
        Started(JobStarted),
        Finished(JobFinished),
    }

    struct Recorder(mpsc::Sender<RecordedJobEvent>);

    impl EventSink for Recorder {
        fn repo_changed(&self, _: RepoChanged) {}
        fn repo_error(&self, _: RepoError) {}
        fn job_started(&self, event: JobStarted) {
            let _ = self.0.send(RecordedJobEvent::Started(event));
        }
        fn job_finished(&self, event: JobFinished) {
            let _ = self.0.send(RecordedJobEvent::Finished(event));
        }
    }

    fn sink() -> (Arc<dyn EventSink>, Receiver<RecordedJobEvent>) {
        let (sender, receiver) = mpsc::channel();
        (Arc::new(Recorder(sender)), receiver)
    }

    fn slow_spec(delay: &str) -> JobSpec {
        let mut spec = JobSpec::new(
            "test",
            "Test job",
            std::env::current_dir().unwrap(),
            vec!["-c".into(), format!("exec sleep {delay}")],
            "Done",
        );
        spec.program = OsString::from("sh");
        spec.timeout = Duration::from_secs(3);
        spec
    }

    fn receive_finished(receiver: &Receiver<RecordedJobEvent>) -> JobFinished {
        loop {
            match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
                RecordedJobEvent::Finished(event) => return event,
                RecordedJobEvent::Started(_) => {}
            }
        }
    }

    #[test]
    fn failure_messages_are_friendly_for_common_git_errors() {
        let spec = JobSpec::new("pull", "Pull main", PathBuf::new(), vec![], "Pulled main");
        assert_eq!(
            friendly_failure(
                &spec,
                "fatal: Not possible to fast-forward, aborting.",
                false
            ),
            "Branch has diverged from upstream; merge or rebase instead"
        );
        assert_eq!(
            friendly_failure(&spec, "fatal: Authentication failed", false),
            "Authentication failed; check your Git credentials"
        );
    }

    #[test]
    fn timeout_failure_is_reported() {
        let spec = JobSpec::new("fetch", "Fetch", PathBuf::new(), vec![], "Fetched");
        assert_eq!(friendly_failure(&spec, "", true), "Git operation timed out");
    }

    #[test]
    fn redacts_url_credentials() {
        for (input, expected) in [
            (
                format!(
                    "fatal: unable to access 'https://user:{}@example.com/x.git'",
                    "secret"
                ),
                "fatal: unable to access 'https://***@example.com/x.git'".to_owned(),
            ),
            (
                "https://token@example.com/x.git".to_owned(),
                "https://***@example.com/x.git".to_owned(),
            ),
            (
                format!(
                    "https://user:{}@one.example/x https://token@two.example/y",
                    "secret"
                ),
                "https://***@one.example/x https://***@two.example/y".to_owned(),
            ),
            (
                "fatal: could not read from remote repository git@github.com:org/repo".to_owned(),
                "fatal: could not read from remote repository git@github.com:org/repo".to_owned(),
            ),
            (
                "No URL in this message".to_owned(),
                "No URL in this message".to_owned(),
            ),
        ] {
            assert_eq!(redact_credentials(&input), expected);
        }
    }

    #[test]
    fn jobs_for_one_repository_run_in_order() {
        let (sink, receiver) = sink();
        let queue = JobQueue::new("one".into(), sink);
        let ids = (0..3)
            .map(|_| queue.enqueue(slow_spec("0.05"), || {}).unwrap())
            .collect::<Vec<_>>();
        let mut observed = Vec::new();
        while observed.len() < 6 {
            match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
                RecordedJobEvent::Started(event) => observed.push(("start", event.job_id)),
                RecordedJobEvent::Finished(event) => observed.push(("finish", event.job_id)),
            }
        }
        assert_eq!(
            observed,
            ids.into_iter()
                .flat_map(|id| [("start", id.clone()), ("finish", id)])
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn different_repository_workers_overlap() {
        let (sink, receiver) = sink();
        let first = JobQueue::new("one".into(), Arc::clone(&sink));
        let second = JobQueue::new("two".into(), sink);
        first.enqueue(slow_spec("0.3"), || {}).unwrap();
        loop {
            if let RecordedJobEvent::Started(event) =
                receiver.recv_timeout(Duration::from_secs(2)).unwrap()
                && event.repo_id == "one"
            {
                break;
            }
        }
        second.enqueue(slow_spec("0.3"), || {}).unwrap();
        let mut second_started = false;
        while !second_started {
            match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
                RecordedJobEvent::Started(event) if event.repo_id == "two" => {
                    second_started = true;
                }
                RecordedJobEvent::Finished(event) if event.repo_id == "one" => {
                    panic!("first repository job finished before the second started");
                }
                _ => {}
            }
        }
        assert!(second_started);
    }

    #[test]
    fn timeout_kills_and_reaps_the_child() {
        let (sink, receiver) = sink();
        let queue = JobQueue::new("timeout".into(), sink);
        let mut spec = slow_spec("10");
        spec.timeout = Duration::from_millis(100);
        queue.enqueue(spec, || {}).unwrap();
        let finished = receive_finished(&receiver);
        assert!(!finished.ok);
        assert_eq!(finished.message, "Git operation timed out");
    }

    #[test]
    fn timeout_also_terminates_transport_children_holding_output_pipes() {
        let (sink, receiver) = sink();
        let queue = JobQueue::new("timeout-tree".into(), sink);
        let mut spec = slow_spec("10");
        spec.args = vec!["-c".into(), "sleep 10 & wait".into()];
        spec.timeout = Duration::from_millis(100);
        let started = Instant::now();
        queue.enqueue(spec, || {}).unwrap();
        let finished = receive_finished(&receiver);
        assert!(!finished.ok);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn failing_push_does_not_expose_url_credentials_in_job_finished() {
        use std::{net::TcpListener, process::Command};

        let directory = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        std::fs::write(directory.path().join("file"), "content").unwrap();
        Command::new("git")
            .args(["add", "file"])
            .current_dir(directory.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "--quiet", "-m", "test"])
            .current_dir(directory.path())
            .status()
            .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let (sink, receiver) = sink();
        let queue = JobQueue::new("redaction".into(), sink);
        let spec = JobSpec::new(
            "push",
            "Push",
            directory.path().to_path_buf(),
            vec![
                "push".into(),
                format!("https://user:{}@127.0.0.1:{port}/x.git", "secret"),
                "HEAD".into(),
            ],
            "Pushed",
        );
        queue.enqueue(spec, || {}).unwrap();
        let finished = receive_finished(&receiver);

        assert!(!finished.ok);
        assert!(!format!("{finished:?}").contains("secret"));
    }

    #[test]
    fn closing_a_queue_cancels_waiting_jobs_but_finishes_the_running_job() {
        let (sink, receiver) = sink();
        let queue = JobQueue::new("closing".into(), sink);
        let running = queue.enqueue(slow_spec("0.2"), || {}).unwrap();
        loop {
            if let RecordedJobEvent::Started(event) =
                receiver.recv_timeout(Duration::from_secs(2)).unwrap()
                && event.job_id == running
            {
                break;
            }
        }
        let queued = queue.enqueue(slow_spec("0.2"), || {}).unwrap();
        queue.cancel_queued();
        let first = receive_finished(&receiver);
        let second = receive_finished(&receiver);
        assert_eq!(first.job_id, running);
        assert!(first.ok);
        assert_eq!(second.job_id, queued);
        assert!(!second.ok);
        assert_eq!(
            second.message,
            "Job cancelled because the repository was closed"
        );
    }
}
