use super::*;
use std::cmp::Ordering;

const BENCHMARK_RUNS: usize = 9;
const BENCHMARK_BATCH_SIZES: [usize; 4] = [1, 32, 64, 128];

#[derive(Clone, Copy)]
struct BenchmarkFixtureSpec {
    total_files: usize,
    rg_candidates: usize,
    matching_files: usize,
    retention_results: usize,
}

fn benchmark_fixture_spec() -> BenchmarkFixtureSpec {
    BenchmarkFixtureSpec {
        total_files: 256,
        rg_candidates: 128,
        matching_files: 32,
        retention_results: 20_000,
    }
}

#[test]
fn benchmark_matrix_covers_required_bounded_comparisons() {
    assert_eq!(BENCHMARK_BATCH_SIZES, [1, 32, 64, 128]);
    assert_eq!(BENCHMARK_RUNS, 9);
    assert!(benchmark_fixture_spec().total_files > benchmark_fixture_spec().rg_candidates);
    assert!(benchmark_fixture_spec().matching_files > 0);
    assert!(benchmark_fixture_spec().retention_results > ResultLimit::TenThousand.get());
}

#[test]
fn search_metrics_record_streaming_worker_boundaries() {
    let metrics = SearchMetrics::default();
    assert_eq!(metrics.candidates_examined, 0);
    assert_eq!(RunOptions::default().max_batch_paths(), RG_BATCH_MAX_PATHS);
}

#[test]
fn streaming_sample_first_result_and_completion_share_origin() {
    let fixture = create_benchmark_fixture();
    let fake = FakeRg::capturing_arguments();
    let mut draft = SearchDraft::advanced(fixture.path().to_path_buf(), SearchScope::RecursiveHere);
    draft.content = "needle".into();
    let mut request = draft.compile(true).unwrap();
    request.rg_program = Some(fake.command.program.clone());
    let sample = streaming_content_sample(request, RG_BATCH_MAX_PATHS);
    assert!(sample.timing.first_us <= sample.timing.wall_us);
    assert!(sample.timing.first_us > 0);
    assert!(fake.capture.exists());
    assert!(fake.arguments().iter().any(|argument| argument == "needle"));
}

#[test]
fn refresh_hits_updates_metadata_rename_marks_and_removes_stale_kinds() {
    let temp = tempfile::tempdir().unwrap();
    let retained = temp.path().join("retained.txt");
    let deleted = temp.path().join("deleted.txt");
    let old = temp.path().join("old.txt");
    let renamed = temp.path().join("renamed.txt");
    let replaced = temp.path().join("replaced.txt");
    for path in [&retained, &deleted, &old, &replaced] {
        fs::write(path, b"x").unwrap();
    }
    let mut hits = [&retained, &deleted, &old, &replaced]
        .into_iter()
        .map(|path| hit_for_test(path.clone(), "txt"))
        .collect::<Vec<_>>();
    hits[0].entry.selected = true;
    hits[2].entry.selected = true;
    fs::write(&retained, b"longer contents").unwrap();
    fs::remove_file(&deleted).unwrap();
    fs::rename(&old, &renamed).unwrap();
    fs::remove_file(&replaced).unwrap();
    fs::create_dir(&replaced).unwrap();

    let summary = refresh_hits(&mut hits, Some((&old, &renamed)));

    assert_eq!(
        summary,
        RefreshSummary {
            retained: 2,
            removed: 2,
            renamed: true
        }
    );
    assert_eq!(hits[0].entry.size, b"longer contents".len() as u64);
    assert!(hits[0].entry.selected);
    assert_eq!(hits[1].entry.path, renamed);
    assert!(hits[1].entry.selected);
}
use chrono::NaiveDateTime;
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs,
    os::unix::ffi::OsStringExt,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
};

struct FakeRg {
    _temp: tempfile::TempDir,
    command: RgCommand,
    capture: PathBuf,
}

impl FakeRg {
    fn install(program: &Path, body: impl AsRef<[u8]>) {
        let staged = program.with_extension("staged");
        let mut file = fs::File::create(&staged).unwrap();
        std::io::Write::write_all(&mut file, body.as_ref()).unwrap();
        file.sync_all().unwrap();
        let mut permissions = file.metadata().unwrap().permissions();
        permissions.set_mode(0o755);
        file.set_permissions(permissions).unwrap();
        drop(file);
        fs::rename(staged, program).unwrap();
        fs::File::open(program.parent().unwrap())
            .unwrap()
            .sync_all()
            .unwrap();
    }

    fn capturing_arguments() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let program = temp.path().join("rg");
        let capture = temp.path().join("arguments");
        Self::install(
                &program,
                format!(
                    "#!/bin/sh\n: > '{}'\nafter_separator=\nfor argument do\n  printf '%s\\0' \"$argument\" >> '{}'\n  if [ \"$after_separator\" = yes ]; then\n    printf '%s\\0' \"$argument\"\n  fi\n  if [ \"$argument\" = -- ]; then after_separator=yes; fi\ndone\n",
                    capture.display(),
                    capture.display()
                ),
            );
        Self {
            _temp: temp,
            command: RgCommand {
                program,
                inject_supervision_error: false,
                inject_cleanup_esrch: false,
                inject_cancel_after_spawn: false,
            },
            capture,
        }
    }

    fn from_script(script: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let program = temp.path().join("rg");
        let capture = temp.path().join("capture");
        Self::install(
            &program,
            script.replace("$CAPTURE", &capture.to_string_lossy()),
        );
        Self {
            _temp: temp,
            command: RgCommand {
                program,
                inject_supervision_error: false,
                inject_cleanup_esrch: false,
                inject_cancel_after_spawn: false,
            },
            capture,
        }
    }

    fn matching_literal_content() -> Self {
        Self::from_script(
                "#!/bin/sh\npattern=\nafter=\nfor argument do\n  if [ \"$after\" = yes ]; then\n    if grep -IqF -- \"$pattern\" \"$argument\"; then printf '%s\\0' \"$argument\"; fi\n  elif [ \"$argument\" = -- ]; then\n    after=yes\n  elif [ -z \"$pattern\" ] && [ \"${argument#--}\" = \"$argument\" ]; then\n    pattern=$argument\n  fi\ndone\n",
            )
    }

    fn arguments(&self) -> Vec<OsString> {
        fs::read(&self.capture)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| OsString::from_vec(argument.to_vec()))
            .collect()
    }
}

fn wait_for_pids(capture: &Path, count: usize) -> Vec<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "rg did not record PIDs"
        );
        if let Ok(contents) = fs::read_to_string(capture) {
            let pids: Vec<_> = contents.lines().map(str::to_owned).collect();
            if pids.len() == count {
                return pids;
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn assert_processes_gone(pids: &[String]) {
    #[cfg(target_os = "linux")]
    for pid in pids {
        assert!(!Path::new("/proc").join(pid).exists(), "PID {pid} remains");
    }
}

fn wait_for_processes_gone(pids: &[String]) {
    #[cfg(target_os = "linux")]
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while pids.iter().any(|pid| Path::new("/proc").join(pid).exists()) {
            assert!(
                std::time::Instant::now() < deadline,
                "escaped writer did not exit after reader closure"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
}

struct TraversalFixture {
    temp: tempfile::TempDir,
}

impl TraversalFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("nested")).unwrap();
        fs::write(temp.path().join("top.txt"), "top").unwrap();
        fs::write(temp.path().join("nested/deep.txt"), "deep").unwrap();
        fs::write(temp.path().join(".hidden.txt"), "hidden").unwrap();
        fs::write(temp.path().join("ignored.txt"), "ignored").unwrap();
        fs::write(temp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        symlink(temp.path(), temp.path().join("loop")).unwrap();
        Self { temp }
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }
}

fn traversal_request(
    root: &Path,
    scope: SearchScope,
    include_ignored_hidden: bool,
) -> CompiledSearch {
    let mut draft = SearchDraft::advanced(root.to_path_buf(), scope);
    draft.name_mode = NameMode::Regex;
    draft.name = r"^(top\.txt|deep\.txt|\.hidden\.txt|ignored\.txt|loop)$".into();
    draft.include_ignored_hidden = include_ignored_hidden;
    draft.compile(true).unwrap()
}

fn run_traversal(request: CompiledSearch) -> (Vec<SearchHit>, SearchCompletion) {
    let running = spawn(request);
    let mut hits = Vec::new();
    loop {
        match running.receiver.recv().unwrap() {
            SearchUpdate::Match(hit) => hits.push(hit),
            SearchUpdate::Skipped(_) => {}
            SearchUpdate::Finished(completion) => return (hits, completion),
            SearchUpdate::Failed(message) => panic!("search failed: {message}"),
        }
    }
}

fn paths(root: &Path, hits: &[SearchHit]) -> BTreeSet<PathBuf> {
    hits.iter()
        .map(|hit| hit.entry.path.strip_prefix(root).unwrap().to_path_buf())
        .collect()
}

fn count_named(hits: &[SearchHit], name: &str) -> usize {
    hits.iter().filter(|hit| hit.entry.name == name).count()
}

fn compiled_name(mode: NameMode, name: &str) -> CompiledSearch {
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.name_mode = mode;
    draft.name = name.into();
    draft.compile(true).unwrap()
}

fn compiled_filters(minimum: &str, maximum: &str, after: &str, before: &str) -> CompiledSearch {
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.name = "report".into();
    draft.minimum_size = minimum.into();
    draft.maximum_size = maximum.into();
    draft.modified_after = after.into();
    draft.modified_before = before.into();
    draft.compile(true).unwrap()
}

fn local_time(value: &str) -> SystemTime {
    let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").unwrap();
    Local.from_local_datetime(&naive).single().unwrap().into()
}

#[test]
fn rg_arguments_literal_mode_are_safe_and_explicit() {
    let fake = FakeRg::capturing_arguments();
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "literal needle".into();
    let request = draft.compile(true).unwrap();
    let candidates = [PathBuf::from("/tmp/one file"), PathBuf::from("-leading")];

    let matches = run_rg_batch_with_command(
        &request,
        &candidates,
        &AtomicBool::new(false),
        &fake.command,
    )
    .unwrap();

    assert_eq!(matches, HashSet::from(candidates.clone()));
    assert_eq!(
        fake.arguments(),
        [
            "--fixed-strings".into(),
            "--null".into(),
            "--files-with-matches".into(),
            "--no-messages".into(),
            "literal needle".into(),
            "--".into(),
            candidates[0].as_os_str().to_owned(),
            candidates[1].as_os_str().to_owned(),
        ]
    );
}

#[test]
fn rg_arguments_regex_mode_omits_fixed_strings() {
    let fake = FakeRg::capturing_arguments();
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle.+thread".into();
    draft.content_mode = ContentMode::Regex;
    let request = draft.compile(true).unwrap();
    let candidates = [PathBuf::from("/tmp/one")];

    run_rg_batch_with_command(
        &request,
        &candidates,
        &AtomicBool::new(false),
        &fake.command,
    )
    .unwrap();

    assert_eq!(
        fake.arguments(),
        [
            "--null".into(),
            "--files-with-matches".into(),
            "--no-messages".into(),
            "needle.+thread".into(),
            "--".into(),
            candidates[0].as_os_str().to_owned(),
        ]
    );
}

#[test]
fn rg_unusual_paths_round_trip_and_binary_files_are_excluded() {
    let fake = FakeRg::matching_literal_content();
    let temp = tempfile::tempdir().unwrap();
    let names = [
        OsString::from("with space"),
        OsString::from("with\ttab"),
        OsString::from("with\nnewline"),
        OsString::from("-leading"),
        OsString::from_vec(vec![b'i', b'n', b'v', 0x80]),
    ];
    let mut paths = Vec::new();
    for name in names {
        let path = temp.path().join(name);
        fs::write(&path, "find this needle").unwrap();
        paths.push(path);
    }
    let binary = temp.path().join("binary");
    fs::write(&binary, b"\0binary contents without the query").unwrap();
    paths.push(binary.clone());
    let mut draft = SearchDraft::quick(temp.path().to_path_buf());
    draft.content = "needle".into();

    let mut request = draft.compile(true).unwrap();
    request.rg_program = Some(fake.command.program.clone());
    let matches = run_rg_batch(&request, &paths, &AtomicBool::new(false)).unwrap();

    assert_eq!(matches.len(), 5);
    assert!(paths[..5].iter().all(|path| matches.contains(path)));
    assert!(!matches.contains(&binary));
}

#[test]
fn rg_cancellation_kills_and_reaps_the_child() {
    let fake = FakeRg::from_script("#!/bin/sh\necho $$ > '$CAPTURE'\nwhile :; do sleep 1; done\n");
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let command = fake.command.clone();
    let started = std::time::Instant::now();
    let worker = thread::spawn(move || {
        run_rg_batch_with_command(
            &request,
            &[PathBuf::from("/tmp/candidate")],
            &worker_cancel,
            &command,
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let pid = loop {
        assert!(std::time::Instant::now() < deadline, "rg did not launch");
        if let Ok(pid) = fs::read_to_string(&fake.capture) {
            if !pid.trim().is_empty() {
                break pid;
            }
        }
        thread::yield_now();
    };
    let pid = pid.trim();
    cancel.store(true, AtomicOrdering::Relaxed);

    assert!(matches!(worker.join().unwrap(), Err(RgError::Cancelled)));
    assert!(started.elapsed() < Duration::from_secs(2));
    #[cfg(target_os = "linux")]
    assert!(!Path::new("/proc").join(pid).exists());
}

#[test]
fn rg_cancellation_kills_descendants_that_inherit_pipes() {
    let fake = FakeRg::from_script(
        "#!/bin/sh\necho $$ > '$CAPTURE'\nsleep 60 &\necho $! >> '$CAPTURE'\nwait\n",
    );
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let command = fake.command.clone();
    let started = std::time::Instant::now();
    let worker = thread::spawn(move || {
        run_rg_batch_with_command(
            &request,
            &[PathBuf::from("/tmp/candidate")],
            &worker_cancel,
            &command,
        )
    });
    let pids = wait_for_pids(&fake.capture, 2);
    cancel.store(true, AtomicOrdering::Relaxed);

    assert!(matches!(worker.join().unwrap(), Err(RgError::Cancelled)));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_processes_gone(&pids);
}

#[test]
fn rg_supervision_error_cleans_up_the_process_group() {
    let fake = FakeRg::from_script(
        "#!/bin/sh\necho $$ > '$CAPTURE'\nsleep 60 &\necho $! >> '$CAPTURE'\nwait\n",
    );
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();
    let mut command = fake.command.clone();
    command.inject_supervision_error = true;
    let started = std::time::Instant::now();
    let worker = thread::spawn(move || {
        run_rg_batch_with_command(
            &request,
            &[PathBuf::from("/tmp/candidate")],
            &AtomicBool::new(false),
            &command,
        )
    });
    let pids = wait_for_pids(&fake.capture, 2);
    let result = worker.join().unwrap();

    assert!(matches!(result, Err(RgError::Failed(message)) if message.contains("supervision")));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_processes_gone(&pids);
}

#[test]
fn rg_normal_completion_does_not_wait_for_escaped_output_holder() {
    if Command::new("setsid").arg("--version").output().is_err() {
        return;
    }
    let fake =
        FakeRg::from_script("#!/bin/sh\nsetsid sh -c 'sleep 1' &\necho $! > '$CAPTURE'\nexit 1\n");
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();
    let started = std::time::Instant::now();

    let result = run_rg_batch_with_command(
        &request,
        &[PathBuf::from("/tmp/candidate")],
        &AtomicBool::new(false),
        &fake.command,
    );

    assert!(matches!(result, Ok(matches) if matches.is_empty()));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn rg_cancellation_does_not_wait_for_escaped_output_holder() {
    if Command::new("setsid").arg("--version").output().is_err() {
        return;
    }
    let fake = FakeRg::from_script(
            "#!/bin/sh\nsetsid sh -c 'sleep 0.1; while printf x; do :; done' &\necho $! > '$CAPTURE'\nwait\n",
        );
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let command = fake.command.clone();
    let worker = thread::spawn(move || {
        run_rg_batch_with_command(
            &request,
            &[PathBuf::from("/tmp/candidate")],
            &worker_cancel,
            &command,
        )
    });
    let pids = wait_for_pids(&fake.capture, 1);
    let started = std::time::Instant::now();
    cancel.store(true, AtomicOrdering::Relaxed);

    assert!(matches!(worker.join().unwrap(), Err(RgError::Cancelled)));
    assert!(started.elapsed() < Duration::from_millis(500));
    wait_for_processes_gone(&pids);
}

#[test]
fn rg_cancellation_race_treats_esrch_after_reap_as_cancelled() {
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();
    let mut command = RgCommand {
        program: PathBuf::from("/bin/true"),
        inject_supervision_error: false,
        inject_cleanup_esrch: false,
        inject_cancel_after_spawn: false,
    };
    command.inject_cleanup_esrch = true;
    command.inject_cancel_after_spawn = true;

    let result = run_rg_batch_with_command(
        &request,
        &[PathBuf::from("/tmp/candidate")],
        &AtomicBool::new(false),
        &command,
    );

    assert!(
        matches!(result, Err(RgError::Cancelled)),
        "unexpected cancellation race result: {result:?}"
    );
}

#[test]
fn rg_rejects_stdout_larger_than_the_current_batch_can_encode() {
    let fake = FakeRg::from_script("#!/bin/sh\nhead -c 70000 /dev/zero\nexit 0\n");
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();

    let result = run_rg_batch_with_command(
        &request,
        &[PathBuf::from("/tmp/candidate")],
        &AtomicBool::new(false),
        &fake.command,
    );

    assert!(
        matches!(result, Err(RgError::Failed(message)) if message.contains("stdout") && message.contains("limit"))
    );
}

#[test]
fn rg_rejects_stderr_larger_than_the_diagnostic_cap() {
    let fake = FakeRg::from_script("#!/bin/sh\nhead -c 70000 /dev/zero >&2\nexit 2\n");
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();

    let result = run_rg_batch_with_command(
        &request,
        &[PathBuf::from("/tmp/candidate")],
        &AtomicBool::new(false),
        &fake.command,
    );

    assert!(
        matches!(result, Err(RgError::Failed(message)) if message.contains("stderr") && message.contains("limit"))
    );
}

#[test]
fn rg_escaped_writer_exits_after_parent_closes_stream_readers() {
    if Command::new("setsid").arg("--version").output().is_err() {
        return;
    }
    let fake = FakeRg::from_script(
            "#!/bin/sh\nsetsid sh -c 'echo $$ > \"$CAPTURE\"; trap \"\" PIPE; while printf x; do :; done; exit 0' &\nexit 1\n",
        );
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();
    let started = std::time::Instant::now();

    let result = run_rg_batch_with_command(
        &request,
        &[PathBuf::from("/tmp/candidate")],
        &AtomicBool::new(false),
        &fake.command,
    );
    let pids = wait_for_pids(&fake.capture, 1);

    assert!(
        matches!(result, Err(RgError::Failed(message)) if message.contains("stdout") && message.contains("limit"))
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    wait_for_processes_gone(&pids);
}

#[test]
fn rg_simultaneous_stream_flood_is_bounded_without_starvation() {
    let fake = FakeRg::from_script(
        "#!/bin/sh\n(head -c 70000 /dev/zero >&2) &\nhead -c 70000 /dev/zero\nwait\n",
    );
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();
    let started = std::time::Instant::now();

    let result = run_rg_batch_with_command(
        &request,
        &[PathBuf::from("/tmp/candidate")],
        &AtomicBool::new(false),
        &fake.command,
    );

    assert!(matches!(result, Err(RgError::Failed(message)) if message.contains("output limit")));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn rg_argument_budget_accepts_exact_limit_and_rejects_one_byte_more() {
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();
    let command = RgCommand::default();
    let budget = RgArgBudget::new(&request, &command).unwrap();
    let exact = PathBuf::from(OsString::from_vec(vec![
        b'x';
        RG_BATCH_MAX_ARG_BYTES
            - budget.fixed_bytes
            - 1
    ]));
    let too_large = PathBuf::from(OsString::from_vec(vec![
        b'x';
        RG_BATCH_MAX_ARG_BYTES
            - budget.fixed_bytes
    ]));

    assert!(budget.can_add(budget.fixed_bytes, &exact));
    assert!(!budget.can_add(budget.fixed_bytes, &too_large));
}

#[test]
fn rg_argument_budget_rejects_oversized_pattern_before_spawn() {
    let fake = FakeRg::capturing_arguments();
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "x".repeat(RG_BATCH_MAX_ARG_BYTES);
    let request = draft.compile(true).unwrap();

    let result = run_rg_batch_with_command(
        &request,
        &[PathBuf::from("/tmp/candidate")],
        &AtomicBool::new(false),
        &fake.command,
    );

    assert!(matches!(result, Err(RgError::Failed(message)) if message.contains("argument limit")));
    assert!(!fake.capture.exists());
}

#[test]
fn rg_batch_applies_count_ceiling_even_when_byte_budget_remains() {
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "needle".into();
    let request = draft.compile(true).unwrap();
    let budget = RgArgBudget::new(&request, &RgCommand::default()).unwrap();
    let mut batch = RgBatch::new(budget);

    for index in 0..RG_BATCH_MAX_PATHS {
        assert!(batch.try_push(PathBuf::from(format!("p{index}"))));
    }
    assert!(!batch.try_push(PathBuf::from("one-too-many")));
    assert!(batch.arg_bytes < RG_BATCH_MAX_ARG_BYTES);
}

#[test]
fn content_search_emits_only_rg_verified_regular_files() {
    let fake = FakeRg::matching_literal_content();
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("matching.txt"), "contains needle").unwrap();
    fs::write(temp.path().join("other.txt"), "nothing relevant").unwrap();
    fs::create_dir(temp.path().join("directory.txt")).unwrap();
    let mut draft = SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::CurrentDirectory);
    draft.name = ".txt".into();
    draft.content = "needle".into();

    let mut request = draft.compile(true).unwrap();
    request.rg_program = Some(fake.command.program.clone());
    let (hits, completion) = run_traversal(request);

    assert_eq!(
        paths(temp.path(), &hits),
        BTreeSet::from(["matching.txt".into()])
    );
    assert_eq!(completion, SearchCompletion::default());
}

#[test]
fn content_failure_preserves_completed_batches_and_reports_incomplete() {
    let fake = FakeRg::from_script(
            "#!/bin/sh\ncount_file='$CAPTURE'\ncount=0\n[ ! -f \"$count_file\" ] || count=$(cat \"$count_file\")\ncount=$((count + 1))\necho \"$count\" > \"$count_file\"\nif [ \"$count\" -eq 1 ]; then\n  after=\n  for argument do\n    if [ \"$after\" = yes ]; then printf '%s\\0' \"$argument\"; fi\n    [ \"$argument\" = -- ] && after=yes\n  done\n  exit 0\nfi\necho 'bad pattern' >&2\nexit 2\n",
        );
    let temp = tempfile::tempdir().unwrap();
    for index in 0..(RG_BATCH_MAX_PATHS + 1) {
        fs::write(temp.path().join(format!("item-{index}.txt")), "needle").unwrap();
    }
    let mut draft = SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::CurrentDirectory);
    draft.name = "item".into();
    draft.content = "needle".into();
    let mut request = draft.compile(true).unwrap();
    request.rg_program = Some(fake.command.program.clone());
    let running = spawn(request);
    let mut matches = 0;
    let mut failure = None;
    let mut completion = None;
    while let Ok(update) = running.receiver.recv() {
        match update {
            SearchUpdate::Match(_) => matches += 1,
            SearchUpdate::Skipped(_) => {}
            SearchUpdate::Failed(message) => failure = Some(message),
            SearchUpdate::Finished(value) => completion = Some(value),
        }
    }

    assert_eq!(matches, RG_BATCH_MAX_PATHS);
    assert!(failure
        .as_deref()
        .is_some_and(|message| message.contains("bad pattern")));
    assert_eq!(
        completion,
        Some(SearchCompletion {
            incomplete: true,
            ..SearchCompletion::default()
        })
    );
}

#[test]
fn empty_search_requires_content_or_a_non_default_filter() {
    let root = PathBuf::from("/tmp/root");
    assert_eq!(
        SearchDraft::advanced(root.clone(), SearchScope::RecursiveHere)
            .compile(true)
            .unwrap_err(),
        SearchValidationError::Unconstrained
    );

    let mut filtered = SearchDraft::advanced(root, SearchScope::RecursiveHere);
    filtered.types = EntryKinds::FILES;
    assert!(filtered.compile(true).is_ok());
}

#[test]
fn size_and_time_bounds_must_be_ordered() {
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.name = "report".into();
    draft.minimum_size = "20 MiB".into();
    draft.maximum_size = "10 MiB".into();
    assert_eq!(
        draft.compile(true).unwrap_err(),
        SearchValidationError::SizeOrder
    );
}

#[test]
fn content_search_requires_ripgrep() {
    let mut draft = SearchDraft::advanced(PathBuf::from("/tmp"), SearchScope::RecursiveHere);
    draft.content = "needle".into();
    assert_eq!(
        draft.compile(false).unwrap_err(),
        SearchValidationError::RipgrepRequired
    );
}

#[test]
fn smart_matching_ranks_exact_prefix_substring_then_fuzzy() {
    let search = compiled_name(NameMode::Smart, "report");
    let exact = search
        .matches_name(Path::new("report"), OsStr::new("report"))
        .unwrap();
    let prefix = search
        .matches_name(Path::new("report-old"), OsStr::new("report-old"))
        .unwrap();
    let contains = search
        .matches_name(Path::new("annual-report"), OsStr::new("annual-report"))
        .unwrap();
    let fuzzy = search
        .matches_name(Path::new("rpeort"), OsStr::new("rpeort"))
        .unwrap();
    assert!(exact < prefix && prefix < contains && contains < fuzzy);
}

#[test]
fn slash_globs_match_relative_paths_but_plain_globs_match_basenames() {
    assert!(compiled_name(NameMode::Glob, "src/*.rs")
        .matches_name(Path::new("src/main.rs"), OsStr::new("main.rs"))
        .is_some());
    assert!(compiled_name(NameMode::Glob, "*.rs")
        .matches_name(Path::new("src/main.rs"), OsStr::new("main.rs"))
        .is_some());
}

#[test]
fn size_filters_exclude_directories_and_time_bounds_are_inclusive() {
    let search = compiled_filters("10", "20", "2026-08-12", "2026-08-12");
    let day_start = local_time("2026-08-12 00:00:00");
    let day_end = local_time("2026-08-12 23:59:59");
    assert!(!search.matches_metadata(EntryKind::Directory, 15, Some(day_start)));
    assert!(search.matches_metadata(EntryKind::File, 10, Some(day_start)));
    assert!(search.matches_metadata(EntryKind::File, 20, Some(day_end)));
    for kind in [EntryKind::Symlink, EntryKind::BlockDevice, EntryKind::Other] {
        assert!(
            search.matches_metadata(kind, 10, Some(day_start)),
            "{kind:?}"
        );
        assert!(search.matches_metadata(kind, 20, Some(day_end)), "{kind:?}");
    }
}

#[test]
fn compile_rejects_invalid_content_regex_before_search_runs() {
    let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
    draft.content = "(".into();
    draft.content_mode = ContentMode::Regex;
    assert!(matches!(
        draft.compile(true),
        Err(SearchValidationError::InvalidPattern {
            mode: "content regex",
            ..
        })
    ));
}

#[test]
fn parsers_accept_supported_forms_and_reject_invalid_values() {
    for value in ["1", "1 KB", "1 KiB", "2 MiB", "3 GiB"] {
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.name = "x".into();
        draft.minimum_size = value.into();
        assert!(draft.compile(true).is_ok(), "{value}");
    }
    for value in ["-1", "1 TiB", "word"] {
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.name = "x".into();
        draft.minimum_size = value.into();
        assert!(
            matches!(
                draft.compile(true),
                Err(SearchValidationError::InvalidSize { .. })
            ),
            "{value}"
        );
    }
    for value in ["2026-08-12", "7d", "30d"] {
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.name = "x".into();
        draft.modified_after = value.into();
        assert!(draft.compile(true).is_ok(), "{value}");
    }
    for value in ["-7d", "7 days", "2026-02-30"] {
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.name = "x".into();
        draft.modified_after = value.into();
        assert!(
            matches!(
                draft.compile(true),
                Err(SearchValidationError::InvalidTime { .. })
            ),
            "{value}"
        );
    }
}

#[test]
fn size_parser_accepts_documented_si_iec_and_decimal_forms() {
    for (raw, expected) in [
        ("500 B", 500),
        ("20 KB", 20_000),
        ("5 MB", 5_000_000),
        ("1.5 GB", 1_500_000_000),
        ("2 GiB", 2_147_483_648),
        ("1KiB", 1_024),
        ("2MiB", 2_097_152),
        ("3GiB", 3_221_225_472),
        ("1.5KiB", 1_536),
        ("0.1KB", 100),
        ("18446744073709551615B", u64::MAX),
    ] {
        assert_eq!(parse_size(raw), Some(Some(expected)), "{raw}");
    }
}

#[test]
fn size_parser_rejects_fractional_bytes_bad_units_and_overflow() {
    for raw in [
        "0.5 B",
        "0.1KiB",
        "1 XB",
        "-1 KB",
        "NaN GB",
        "18446744073709551616 B",
        "18014398509481984KiB",
    ] {
        assert_eq!(parse_size(raw), None, "{raw}");
    }
}

#[test]
fn invalid_glob_and_regex_are_validation_errors() {
    for (mode, pattern) in [(NameMode::Glob, "["), (NameMode::Regex, "(")] {
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.name_mode = mode;
        draft.name = pattern.into();
        assert!(matches!(
            draft.compile(true),
            Err(SearchValidationError::InvalidPattern { .. })
        ));
    }
}

#[test]
fn smart_matching_handles_empty_unicode_long_and_case_only_inputs() {
    let cases = [
        ("", "anything", true),
        ("RÉSUMÉ", "résumé", true),
        ("report", "REPORT", true),
        ("abcdefghijabcdefghij", "abcdefghijabcdefghij-extra", true),
        ("report", "unrelated", false),
    ];
    for (query, candidate, expected) in cases {
        let mut draft = SearchDraft::quick(PathBuf::from("/tmp"));
        draft.name = query.into();
        if query.is_empty() {
            draft.types = EntryKinds::FILES;
        }
        let search = draft.compile(true).unwrap();
        assert_eq!(
            search
                .matches_name(Path::new(candidate), OsStr::new(candidate))
                .is_some(),
            expected
        );
    }
}

#[test]
fn smart_matching_uses_full_unicode_case_folding() {
    let search = compiled_name(NameMode::Smart, "Straße");
    assert!(search
        .matches_name(Path::new("STRASSE"), OsStr::new("STRASSE"))
        .is_some());
}

#[test]
fn invalid_utf8_basenames_do_not_collide_through_replacement_characters() {
    let invalid_a = OsString::from_vec(vec![b'a', 0x80]);
    let invalid_b = OsString::from_vec(vec![b'a', 0x81]);
    let replacement = compiled_name(NameMode::Smart, "a�");
    assert!(replacement
        .matches_name(Path::new(&invalid_a), &invalid_a)
        .is_none());
    assert!(replacement
        .matches_name(Path::new(&invalid_b), &invalid_b)
        .is_none());

    let glob = compiled_name(NameMode::Glob, "a�");
    assert!(glob
        .matches_name(Path::new(&invalid_a), &invalid_a)
        .is_none());
    assert!(glob
        .matches_name(Path::new(&invalid_b), &invalid_b)
        .is_none());

    let regex = compiled_name(NameMode::Regex, "^a�$");
    assert!(regex
        .matches_name(Path::new(&invalid_a), &invalid_a)
        .is_none());
    assert!(regex
        .matches_name(Path::new(&invalid_b), &invalid_b)
        .is_none());
}

#[test]
fn hit_ties_use_preserved_path_basenames_before_full_paths() {
    let rank = MatchRank {
        tier: 0,
        penalty: 0,
    };
    let first_name = OsString::from_vec(vec![b'a', 0x80]);
    let second_name = OsString::from_vec(vec![b'a', 0x81]);
    let mut first = PathBuf::from("/z");
    first.push(&first_name);
    let mut second = PathBuf::from("/a");
    second.push(&second_name);
    let make_hit = |path: PathBuf, display_name: &str| {
        SearchHit::new(
            FileEntry {
                path,
                name: display_name.into(),
                kind: EntryKind::File,
                size: 0,
                mode: 0,
                modified: None,
                selected: false,
            },
            rank,
        )
    };
    let first = make_hit(first, "same replacement display");
    let second = make_hit(second, "same replacement display");
    assert!(first < second);
}

#[test]
fn traversal_current_directory_does_not_descend() {
    let fixture = TraversalFixture::new();
    let (hits, completion) = run_traversal(traversal_request(
        fixture.root(),
        SearchScope::CurrentDirectory,
        false,
    ));

    assert_eq!(
        paths(fixture.root(), &hits),
        BTreeSet::from(["top.txt".into(), "loop".into()])
    );
    assert_eq!(count_named(&hits, "deep.txt"), 0);
    assert_eq!(
        hits.iter()
            .find(|hit| hit.entry.name == "loop")
            .unwrap()
            .entry
            .kind,
        EntryKind::Symlink
    );
    assert_eq!(completion, SearchCompletion::default());
}

#[test]
fn traversal_current_directory_symlink_filter_returns_loop_without_following_it() {
    let fixture = TraversalFixture::new();
    let mut request = traversal_request(fixture.root(), SearchScope::CurrentDirectory, false);
    request.types = EntryKinds::SYMLINKS;
    let (hits, completion) = run_traversal(request);

    assert_eq!(
        paths(fixture.root(), &hits),
        BTreeSet::from(["loop".into()])
    );
    assert_eq!(count_named(&hits, "deep.txt"), 0);
    assert_eq!(completion, SearchCompletion::default());
}

#[test]
fn traversal_recursive_honors_ignores_and_does_not_follow_symlinks() {
    let fixture = TraversalFixture::new();
    let (hits, completion) = run_traversal(traversal_request(
        fixture.root(),
        SearchScope::RecursiveHere,
        false,
    ));

    assert_eq!(
        paths(fixture.root(), &hits),
        BTreeSet::from(["top.txt".into(), "nested/deep.txt".into(), "loop".into()])
    );
    assert_eq!(count_named(&hits, "deep.txt"), 1);
    assert_eq!(completion, SearchCompletion::default());
}

#[test]
fn traversal_include_ignored_hidden_disables_all_ignore_filters() {
    let fixture = TraversalFixture::new();
    let (hits, completion) = run_traversal(traversal_request(
        fixture.root(),
        SearchScope::RecursiveHere,
        true,
    ));
    let paths = paths(fixture.root(), &hits);

    assert!(paths.contains(Path::new(".hidden.txt")));
    assert!(paths.contains(Path::new("ignored.txt")));
    assert_eq!(count_named(&hits, "deep.txt"), 1);
    assert_eq!(completion, SearchCompletion::default());
}

#[test]
fn traversal_filesystem_prunes_virtual_trees_but_keeps_run_media() {
    assert!(!filesystem_entry_allowed(Path::new("/proc")));
    assert!(!filesystem_entry_allowed(Path::new("/proc/123/status")));
    assert!(!filesystem_entry_allowed(Path::new("/sys/class")));
    assert!(!filesystem_entry_allowed(Path::new("/dev/pts")));
    assert!(filesystem_entry_allowed(Path::new("/run")));
    assert!(!filesystem_entry_allowed(Path::new("/run/lock")));
    assert!(filesystem_entry_allowed(Path::new("/run/media")));
    assert!(filesystem_entry_allowed(Path::new("/run/media/user/disk")));
    assert!(filesystem_entry_allowed(Path::new("/home/user")));
}

#[test]
fn bounded_results_stop_at_the_selected_limit_and_report_truncation() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..8 {
        fs::write(temp.path().join(format!("item-{index}.txt")), "item").unwrap();
    }
    let mut draft = SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::RecursiveHere);
    draft.name = "item".into();
    let mut request = draft.compile(true).unwrap();
    request.result_limit_override = Some(3);

    let (hits, completion) = run_traversal(request);

    assert_eq!(hits.len(), 3);
    assert_eq!(
        completion,
        SearchCompletion {
            truncated: true,
            ..SearchCompletion::default()
        }
    );
}

#[test]
fn cancellation_pre_set_before_traversal_emits_no_matches() {
    let fixture = TraversalFixture::new();
    let request = traversal_request(fixture.root(), SearchScope::RecursiveHere, true);
    let (match_sender, match_receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
    let (control_sender, control_receiver) = mpsc::channel();
    let receiver = SearchReceiver::new(match_receiver, control_receiver);
    let cancel = Arc::new(AtomicBool::new(true));

    run(request, match_sender, control_sender, cancel);
    let mut updates = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(update) => updates.push(update),
            Err(TryRecvError::Empty) => thread::yield_now(),
            Err(TryRecvError::Disconnected) => break,
        }
    }

    assert!(updates
        .iter()
        .all(|update| !matches!(update, SearchUpdate::Match(_))));
    assert!(matches!(
        updates.last(),
        Some(SearchUpdate::Finished(SearchCompletion {
            cancelled: true,
            ..
        }))
    ));
}

#[test]
fn cancellation_after_first_update_completes_promptly() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..1_000 {
        fs::write(temp.path().join(format!("item-{index}.txt")), "item").unwrap();
    }
    let mut request = SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::RecursiveHere);
    request.name = "item".into();
    let running = spawn(request.compile(true).unwrap());
    let started = std::time::Instant::now();
    assert!(matches!(
        running.receiver.recv().unwrap(),
        SearchUpdate::Match(_)
    ));
    running.cancel.store(true, AtomicOrdering::Relaxed);

    let completion = loop {
        match running
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
        {
            SearchUpdate::Finished(completion) => break completion,
            SearchUpdate::Failed(message) => panic!("search failed: {message}"),
            SearchUpdate::Match(_) | SearchUpdate::Skipped(_) => {}
        }
    };
    assert!(completion.cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn cancellation_exits_when_the_update_queue_is_saturated() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..(UPDATE_QUEUE_CAPACITY + 32) {
        fs::write(temp.path().join(format!("item-{index}.txt")), "item").unwrap();
    }
    let mut draft = SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::RecursiveHere);
    draft.name = "item".into();
    let mut request = draft.compile(true).unwrap();
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    request.send_attempt_counter = Some(Arc::clone(&attempts));
    let (sender, match_receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
    let (control_sender, control_receiver) = mpsc::channel();
    let receiver = SearchReceiver::new(match_receiver, control_receiver);
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let (done_sender, done_receiver) = mpsc::channel();
    thread::spawn(move || {
        run(request, sender, control_sender, worker_cancel);
        done_sender.send(()).unwrap();
    });

    let saturation_deadline = std::time::Instant::now() + Duration::from_secs(2);
    while attempts.load(AtomicOrdering::Relaxed) <= UPDATE_QUEUE_CAPACITY {
        assert!(
            std::time::Instant::now() < saturation_deadline,
            "worker did not saturate the update queue"
        );
        thread::yield_now();
    }
    cancel.store(true, AtomicOrdering::Relaxed);

    done_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut matches = 0;
    let completion = loop {
        match receiver.try_recv() {
            Ok(SearchUpdate::Match(_)) => matches += 1,
            Ok(SearchUpdate::Skipped(_)) => {}
            Ok(SearchUpdate::Finished(completion)) => break completion,
            Ok(SearchUpdate::Failed(message)) => panic!("search failed: {message}"),
            Err(mpsc::TryRecvError::Empty) => thread::yield_now(),
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("receiver disconnected before completion")
            }
        }
    };
    assert_eq!(matches, UPDATE_QUEUE_CAPACITY);
    assert!(completion.cancelled);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
}

#[test]
fn saturated_receiver_drains_matches_then_aggregate_skip_and_finished() {
    let (match_sender, match_receiver) = mpsc::sync_channel(UPDATE_QUEUE_CAPACITY);
    let (control_sender, control_receiver) = mpsc::channel();
    let receiver = SearchReceiver::new(match_receiver, control_receiver);
    for index in 0..UPDATE_QUEUE_CAPACITY {
        let path = PathBuf::from(format!("item-{index}.txt"));
        match_sender
            .send(SearchUpdate::Match(SearchHit::new(
                FileEntry {
                    name: path.to_string_lossy().into_owned(),
                    path,
                    kind: EntryKind::File,
                    size: 0,
                    mode: 0,
                    modified: None,
                    selected: false,
                },
                MatchRank {
                    tier: 0,
                    penalty: 0,
                },
            )))
            .unwrap();
    }
    control_sender.send(SearchUpdate::Skipped(7)).unwrap();
    control_sender
        .send(SearchUpdate::Finished(SearchCompletion {
            truncated: true,
            incomplete: true,
            ..SearchCompletion::default()
        }))
        .unwrap();
    drop(match_sender);
    drop(control_sender);

    for _ in 0..UPDATE_QUEUE_CAPACITY {
        assert!(matches!(receiver.try_recv(), Ok(SearchUpdate::Match(_))));
    }
    assert!(matches!(receiver.try_recv(), Ok(SearchUpdate::Skipped(7))));
    assert!(matches!(
        receiver.try_recv(),
        Ok(SearchUpdate::Finished(SearchCompletion {
            truncated: true,
            incomplete: true,
            ..
        }))
    ));
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
}

#[test]
fn saturated_normal_completion_preserves_truncation() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..UPDATE_QUEUE_CAPACITY {
        fs::write(temp.path().join(format!("item-{index}.txt")), "item").unwrap();
    }
    let mut draft = SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::RecursiveHere);
    draft.name = "item".into();
    let mut request = draft.compile(true).unwrap();
    request.result_limit_override = Some(UPDATE_QUEUE_CAPACITY);
    let running = spawn(request);

    let mut matches = 0;
    let completion = loop {
        match running
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
        {
            SearchUpdate::Match(_) => matches += 1,
            SearchUpdate::Skipped(_) => {}
            SearchUpdate::Finished(completion) => break completion,
            SearchUpdate::Failed(message) => panic!("search failed: {message}"),
        }
    };

    assert_eq!(matches, UPDATE_QUEUE_CAPACITY);
    assert!(completion.truncated);
    assert!(!completion.cancelled);
}

#[test]
fn metadata_rejections_do_not_construct_file_entries() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("too-small.txt"), "x").unwrap();
    let mut draft = SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::CurrentDirectory);
    draft.name = "too-small".into();
    draft.minimum_size = "2".into();
    let mut request = draft.compile(true).unwrap();
    let constructions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    request.construction_counter = Some(Arc::clone(&constructions));

    let (hits, completion) = run_traversal(request);

    assert!(hits.is_empty());
    assert_eq!(completion, SearchCompletion::default());
    assert_eq!(constructions.load(AtomicOrdering::Relaxed), 0);
}

#[test]
fn cloned_compiled_search_preserves_request_and_matching() {
    let root = PathBuf::from("/tmp/search-clone");
    let mut draft = SearchDraft::advanced(root.clone(), SearchScope::RecursiveHere);
    draft.name = "Report".into();
    draft.types = EntryKinds::FILES;
    draft.minimum_size = "10".into();
    draft.result_limit = ResultLimit::TenThousand;
    let request = draft.compile(true).unwrap();
    let cloned = request.clone();

    assert_eq!(cloned.root(), root);
    assert_eq!(cloned.scope(), SearchScope::RecursiveHere);
    assert_eq!(cloned.result_limit(), ResultLimit::TenThousand);
    assert_eq!(
        cloned.matches_name(Path::new("Report.txt"), OsStr::new("Report.txt")),
        request.matches_name(Path::new("Report.txt"), OsStr::new("Report.txt"))
    );
    assert_eq!(
        cloned.matches_metadata(EntryKind::File, 10, None),
        request.matches_metadata(EntryKind::File, 10, None)
    );
    assert_eq!(
        cloned.matches_metadata(EntryKind::Directory, 10, None),
        request.matches_metadata(EntryKind::Directory, 10, None)
    );
}

#[test]
fn missing_root_is_reported_as_incomplete() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing");
    let mut draft = SearchDraft::advanced(missing, SearchScope::RecursiveHere);
    draft.name = "anything".into();

    let (hits, completion) = run_traversal(draft.compile(true).unwrap());

    assert!(hits.is_empty());
    assert!(completion.incomplete);
}

#[test]
fn content_requests_do_not_emit_unverified_filename_matches() {
    let fake = FakeRg::matching_literal_content();
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("report.txt"), "not the requested content").unwrap();
    let mut draft = SearchDraft::advanced(temp.path().to_path_buf(), SearchScope::CurrentDirectory);
    draft.name = "report".into();
    draft.content = "needle".into();

    let mut request = draft.compile(true).unwrap();
    request.rg_program = Some(fake.command.program.clone());
    let (hits, completion) = run_traversal(request);

    assert!(hits.is_empty());
    assert_eq!(completion, SearchCompletion::default());
}

#[test]
fn type_mask_toggles_and_filters_each_entry_kind() {
    let kinds = [
        EntryKind::File,
        EntryKind::Directory,
        EntryKind::Symlink,
        EntryKind::BlockDevice,
        EntryKind::Other,
    ];
    for selected in kinds {
        let mut mask = EntryKinds::ANY;
        mask.toggle(selected);
        for candidate in kinds {
            assert_eq!(mask.contains(candidate), selected == candidate);
        }
        mask.toggle(selected);
        assert_eq!(mask, EntryKinds::ANY);
    }
}

#[derive(Clone, Copy, Debug)]
struct BenchmarkSample {
    wall_us: u128,
    cpu_us: u128,
    first_us: u128,
}

fn process_cpu_us() -> u128 {
    let stat = fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let Some((_, fields)) = stat.rsplit_once(") ") else {
        return 0;
    };
    let values = fields.split_whitespace().collect::<Vec<_>>();
    // /proc/self/stat fields 14-17 are this process's utime/stime plus
    // cutime/cstime accumulated from waited-for children, including rg.
    let ticks = [11, 12, 13, 14]
        .into_iter()
        .filter_map(|index| values.get(index)?.parse::<u128>().ok())
        .sum::<u128>();
    static TICKS_PER_SECOND: std::sync::OnceLock<u128> = std::sync::OnceLock::new();
    let hz = *TICKS_PER_SECOND.get_or_init(|| {
        Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(100)
    });
    ticks.saturating_mul(1_000_000) / hz
}

fn median(mut values: Vec<u128>) -> u128 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn median_sample(samples: &[BenchmarkSample]) -> BenchmarkSample {
    BenchmarkSample {
        wall_us: median(samples.iter().map(|sample| sample.wall_us).collect()),
        cpu_us: median(samples.iter().map(|sample| sample.cpu_us).collect()),
        first_us: median(samples.iter().map(|sample| sample.first_us).collect()),
    }
}

fn create_benchmark_fixture() -> tempfile::TempDir {
    let spec = benchmark_fixture_spec();
    let temp = tempfile::tempdir().unwrap();
    for index in 0..spec.total_files {
        let directory = temp.path().join(format!("group-{:02}", index % 16));
        fs::create_dir_all(&directory).unwrap();
        let candidate = index < spec.rg_candidates;
        let name = if candidate {
            format!("report-{index:04}.txt")
        } else {
            format!("other-{index:04}.txt")
        };
        let contents = if index < spec.matching_files {
            "deterministic benchmark needle\n"
        } else {
            "deterministic benchmark haystack\n"
        };
        fs::write(directory.join(name), contents).unwrap();
    }
    temp
}

fn filename_sample(root: &Path) -> (BenchmarkSample, usize) {
    let mut draft = SearchDraft::advanced(root.to_path_buf(), SearchScope::RecursiveHere);
    draft.name = "report".into();
    let cpu_started = process_cpu_us();
    let started = std::time::Instant::now();
    let running = spawn(draft.compile(true).unwrap());
    let mut first = None;
    let mut count = 0;
    loop {
        match running.receiver.recv().unwrap() {
            SearchUpdate::Match(_) => {
                first.get_or_insert_with(|| started.elapsed().as_micros());
                count += 1;
            }
            SearchUpdate::Skipped(_) => {}
            SearchUpdate::Finished(completion) => {
                assert_eq!(completion, SearchCompletion::default());
                break;
            }
            SearchUpdate::Failed(message) => panic!("search failed: {message}"),
        }
    }
    (
        BenchmarkSample {
            wall_us: started.elapsed().as_micros(),
            cpu_us: process_cpu_us().saturating_sub(cpu_started),
            first_us: first.expect("fixture must produce a filename result"),
        },
        count,
    )
}

#[derive(Clone, Copy, Debug)]
struct StreamingSample {
    timing: BenchmarkSample,
    candidates_examined: usize,
    candidates_passed: usize,
    max_batch_paths: usize,
    max_batch_bytes: usize,
    subprocesses: usize,
    matches: usize,
    retained_results: usize,
}

fn streaming_content_sample(request: CompiledSearch, batch_size: usize) -> StreamingSample {
    let metrics = Arc::new(std::sync::Mutex::new(SearchMetrics::default()));
    let cpu_started = process_cpu_us();
    let started = std::time::Instant::now();
    let running = spawn_with_options(
        request,
        RunOptions {
            max_batch_paths_override: Some(batch_size),
        },
        Some(Arc::clone(&metrics)),
    );
    let mut retained_results = 0;
    let mut first_result_us = None;
    loop {
        match running.receiver.recv().unwrap() {
            SearchUpdate::Match(_) => {
                first_result_us.get_or_insert_with(|| started.elapsed().as_micros());
                retained_results += 1;
            }
            SearchUpdate::Skipped(_) => {}
            SearchUpdate::Finished(completion) => {
                assert_eq!(completion, SearchCompletion::default());
                break;
            }
            SearchUpdate::Failed(message) => panic!("search failed: {message}"),
        }
    }
    let metrics = metrics.lock().unwrap();
    StreamingSample {
        timing: BenchmarkSample {
            wall_us: started.elapsed().as_micros(),
            cpu_us: process_cpu_us().saturating_sub(cpu_started),
            first_us: first_result_us.expect("fixture must deliver a match"),
        },
        candidates_examined: metrics.candidates_examined,
        candidates_passed: metrics.candidates_passed_metadata,
        max_batch_paths: metrics.max_batch_paths,
        max_batch_bytes: metrics.max_batch_bytes,
        subprocesses: metrics.rg_subprocesses,
        matches: metrics.matches,
        retained_results,
    }
}

fn cancellation_sample(
    request: &CompiledSearch,
    candidates: &[PathBuf],
    batch_size: usize,
) -> u128 {
    let fake = FakeRg::from_script(
        "#!/bin/sh\necho $$ > '$CAPTURE'\nsleep 60 &\necho $! >> '$CAPTURE'\nwait\n",
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let request = request.clone();
    let command = fake.command.clone();
    let batch = candidates[..batch_size].to_vec();
    let worker = thread::spawn(move || {
        run_rg_batch_with_command(&request, &batch, &worker_cancel, &command)
    });
    let pids = wait_for_pids(&fake.capture, 2);
    let started = std::time::Instant::now();
    cancel.store(true, AtomicOrdering::Relaxed);
    assert!(matches!(worker.join().unwrap(), Err(RgError::Cancelled)));
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(2));
    assert_processes_gone(&pids);
    elapsed.as_micros()
}

fn real_rg_cancellation_sample(request: CompiledSearch) -> Option<u128> {
    let metrics = Arc::new(std::sync::Mutex::new(SearchMetrics::default()));
    let running = spawn_with_options(request, RunOptions::default(), Some(Arc::clone(&metrics)));
    let observation_deadline = std::time::Instant::now() + Duration::from_millis(100);
    let observed_pid = loop {
        if let Some(pid) = metrics.lock().unwrap().rg_leader_pid {
            if Path::new("/proc").join(pid.to_string()).exists() {
                break pid;
            }
        }
        match running.receiver.try_recv() {
            Ok(SearchUpdate::Finished(_)) | Err(TryRecvError::Disconnected) => return None,
            Ok(SearchUpdate::Failed(message)) => panic!("real rg failed: {message}"),
            Ok(SearchUpdate::Match(_) | SearchUpdate::Skipped(_)) | Err(TryRecvError::Empty) => {}
        }
        if std::time::Instant::now() >= observation_deadline {
            running.cancel.store(true, AtomicOrdering::Relaxed);
            drain_cancelled_search(&running.receiver);
            return None;
        }
        thread::yield_now();
    };
    let started = std::time::Instant::now();
    running.cancel.store(true, AtomicOrdering::Relaxed);
    let completion = loop {
        match running
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
        {
            SearchUpdate::Match(_) | SearchUpdate::Skipped(_) => {}
            SearchUpdate::Failed(message) => panic!("real rg cancellation failed: {message}"),
            SearchUpdate::Finished(completion) => break completion,
        }
    };
    assert!(completion.cancelled);
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(2));
    assert!(
        !Path::new("/proc").join(observed_pid.to_string()).exists(),
        "real rg leader remains"
    );
    Some(elapsed.as_micros())
}

fn drain_cancelled_search(receiver: &SearchReceiver) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!remaining.is_zero(), "cancelled retry did not terminate");
        match receiver.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(SearchUpdate::Finished(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(SearchUpdate::Match(_) | SearchUpdate::Skipped(_) | SearchUpdate::Failed(_))
            | Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// Manual baseline (2026-08-12, release build, nine warm-cache runs):
/// first result 1,075 us; completion 1,086 us on the 256-file fixture.
/// Compare trends, not absolute timings, across machines and filesystems.
#[test]
#[ignore]
fn benchmark_search_filename() {
    let fixture = create_benchmark_fixture();
    let mut samples = Vec::with_capacity(BENCHMARK_RUNS);
    for _ in 0..BENCHMARK_RUNS {
        let (sample, matches) = filename_sample(fixture.path());
        assert_eq!(matches, benchmark_fixture_spec().rg_candidates);
        samples.push(sample);
    }
    let sample = median_sample(&samples);
    eprintln!(
            "PERF search_filename_first_result_us={} search_filename_complete_us={} search_filename_cpu_us={}",
            sample.first_us, sample.wall_us, sample.cpu_us
        );
}

/// Manual baseline (2026-08-12, release, nine runs): filtered batch 128
/// completed end-to-end in 11,621 us with 128 candidates/one subprocess;
/// unpruned batch 128 took 22,129 us with 256 candidates/two subprocesses.
/// Executes every required bounded batch size on the identical fixture;
/// each of nine rounds rotates the eight mode/batch cases to avoid a fixed
/// warm-cache ordering advantage.
#[test]
#[ignore]
fn benchmark_search_content_comparisons() {
    assert!(
        ripgrep_available(),
        "ripgrep is required for this benchmark"
    );
    let fixture = create_benchmark_fixture();
    let spec = benchmark_fixture_spec();
    let mut draft = SearchDraft::advanced(fixture.path().to_path_buf(), SearchScope::RecursiveHere);
    draft.name = "report".into();
    draft.content = "needle".into();
    draft.types = EntryKinds::FILES;
    draft.minimum_size = "1".into();
    draft.maximum_size = "64".into();
    draft.modified_after = "1970-01-01".into();
    let filtered_request = draft.compile(true).unwrap();
    let mut unpruned_draft =
        SearchDraft::advanced(fixture.path().to_path_buf(), SearchScope::RecursiveHere);
    unpruned_draft.content = "needle".into();
    let unpruned_request = unpruned_draft.compile(true).unwrap();
    let cases = [
        ("filtered", filtered_request),
        ("unpruned", unpruned_request),
    ];
    let mut all_samples = vec![vec![Vec::<StreamingSample>::new(); 4]; 2];
    for round in 0..BENCHMARK_RUNS {
        for offset in 0..8 {
            let case_index = (round + offset) % 8;
            let mode = case_index / 4;
            let batch = case_index % 4;
            let sample =
                streaming_content_sample(cases[mode].1.clone(), BENCHMARK_BATCH_SIZES[batch]);
            all_samples[mode][batch].push(sample);
        }
    }

    for (mode, (label, _)) in cases.iter().enumerate() {
        for (batch, batch_size) in BENCHMARK_BATCH_SIZES.iter().copied().enumerate() {
            let samples = &all_samples[mode][batch];
            let sample = median_sample(
                &samples
                    .iter()
                    .map(|sample| sample.timing)
                    .collect::<Vec<_>>(),
            );
            let representative = samples[BENCHMARK_RUNS / 2];
            let expected_candidates = if *label == "filtered" {
                spec.rg_candidates
            } else {
                spec.total_files
            };
            assert_eq!(representative.candidates_passed, expected_candidates);
            assert_eq!(representative.matches, spec.matching_files);
            assert_eq!(representative.retained_results, spec.matching_files);
            assert!(representative.max_batch_paths <= batch_size);
            assert!(representative.max_batch_bytes <= RG_BATCH_MAX_ARG_BYTES);
            eprintln!(
                    "PERF search_streaming mode={label} batch_size={batch_size} wall_us={} cpu_us={} candidates_examined={} candidates_passed={} max_batch_paths={} max_batch_bytes={} subprocesses={} matches={} retained_results={} retained_proxy_bytes={} first_result_us={} completion_us={}",
                    sample.wall_us,
                    sample.cpu_us,
                    representative.candidates_examined,
                    representative.candidates_passed,
                    representative.max_batch_paths,
                    representative.max_batch_bytes,
                    representative.subprocesses,
                    representative.matches,
                    representative.retained_results,
                    representative.retained_results * std::mem::size_of::<SearchHit>(),
                    sample.first_us,
                    sample.wall_us
                );
            if *label == "filtered" && batch_size == RG_BATCH_MAX_PATHS {
                eprintln!(
                    "PERF search_filtered_candidates={} search_filtered_complete_us={}",
                    representative.candidates_passed, sample.wall_us
                );
                eprintln!(
                    "PERF search_content_batches={} search_content_complete_us={}",
                    representative.subprocesses, sample.wall_us
                );
            }
        }
    }

    let unpruned = (0..spec.total_files)
        .map(|index| {
            fixture
                .path()
                .join(format!("group-{:02}", index % 16))
                .join(if index < spec.rg_candidates {
                    format!("report-{index:04}.txt")
                } else {
                    format!("other-{index:04}.txt")
                })
        })
        .collect::<Vec<_>>();
    for batch_size in BENCHMARK_BATCH_SIZES {
        let mut samples = Vec::with_capacity(BENCHMARK_RUNS);
        for _ in 0..BENCHMARK_RUNS {
            samples.push(cancellation_sample(&cases[1].1, &unpruned, batch_size));
        }
        let cancel_us = median(samples);
        eprintln!(
                "PERF search_cancel batch_size={batch_size} cancel_to_reaped_us={cancel_us} candidates_in_child={batch_size}"
            );
    }

    let real_cancel_root = fixture.path().join("real-rg-cancel");
    fs::create_dir(&real_cancel_root).unwrap();
    let contents = vec![b'x'; 512 * 1024];
    for index in 0..RG_BATCH_MAX_PATHS {
        fs::write(
            real_cancel_root.join(format!("large-{index:03}.txt")),
            &contents,
        )
        .unwrap();
    }
    let mut real_cancel_draft = SearchDraft::advanced(real_cancel_root, SearchScope::RecursiveHere);
    real_cancel_draft.content = "needle-not-present".into();
    let real_cancel_request = real_cancel_draft.compile(true).unwrap();
    let mut real_cancel_samples = Vec::with_capacity(BENCHMARK_RUNS);
    let retry_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while real_cancel_samples.len() < BENCHMARK_RUNS {
        assert!(
            std::time::Instant::now() < retry_deadline,
            "could not observe live real rg child for cancellation sample"
        );
        if let Some(sample) = real_rg_cancellation_sample(real_cancel_request.clone()) {
            real_cancel_samples.push(sample);
        }
    }
    eprintln!(
            "PERF search_cancel_real_rg batch_size=128 cancel_to_finished_us={} fixture_bytes={} child_cleanup=verified_where_observable",
            median(real_cancel_samples),
            contents.len() * RG_BATCH_MAX_PATHS
        );
}

fn retention_sample(root: &Path, limit: usize) -> (BenchmarkSample, usize) {
    let mut draft = SearchDraft::advanced(root.to_path_buf(), SearchScope::RecursiveHere);
    draft.name = "result".into();
    let mut request = draft.compile(true).unwrap();
    request.result_limit_override = Some(limit);
    let cpu = process_cpu_us();
    let started = std::time::Instant::now();
    let (hits, completion) = run_traversal(request);
    assert_eq!(completion.truncated, hits.len() == limit);
    (
        BenchmarkSample {
            wall_us: started.elapsed().as_micros(),
            cpu_us: process_cpu_us().saturating_sub(cpu),
            first_us: 0,
        },
        hits.len(),
    )
}

/// Manual baseline (2026-08-12, release, nine runs): production-bounded
/// 1,000 results used a 24,000-byte retained-structure proxy and completed
/// in 4,268 us versus 480,000 bytes/82,722 us for real collect-all traversal.
/// Contrasts the production 1,000 cap with a test-only collect-all limit.
#[test]
#[ignore]
fn benchmark_search_streaming_retention_comparison() {
    let fixture = create_benchmark_fixture();
    let retention_root = fixture.path().join("retention");
    fs::create_dir(&retention_root).unwrap();
    for index in 0..benchmark_fixture_spec().retention_results {
        fs::write(retention_root.join(format!("result-{index:05}")), []).unwrap();
    }
    let mut bounded_samples = Vec::new();
    let mut collected_samples = Vec::new();
    let mut bounded_retained = 0;
    let mut collected_retained = 0;
    for _ in 0..BENCHMARK_RUNS {
        let (sample, retained) = retention_sample(&retention_root, ResultLimit::OneThousand.get());
        bounded_samples.push(sample);
        bounded_retained = retained;
        let (sample, retained) = retention_sample(
            &retention_root,
            benchmark_fixture_spec().retention_results + 1,
        );
        collected_samples.push(sample);
        collected_retained = retained;
    }
    let bounded = median_sample(&bounded_samples);
    let collected = median_sample(&collected_samples);
    for (mode, sample, retained) in [
        ("bounded", bounded, bounded_retained),
        ("collect_all", collected, collected_retained),
    ] {
        let retained_proxy_bytes = retained * std::mem::size_of::<PathBuf>();
        eprintln!(
                "PERF search_retention mode={mode} wall_us={} cpu_us={} retained_results={retained} retained_proxy_bytes={retained_proxy_bytes} first_result_us=0 completion_us={}",
                sample.wall_us, sample.cpu_us, sample.wall_us
            );
    }
}

#[test]
fn hits_sort_by_rank_case_folded_name_then_absolute_path() {
    fn hit(name: &str, path: &str, rank: MatchRank) -> SearchHit {
        SearchHit::new(
            FileEntry {
                path: PathBuf::from(path),
                name: name.into(),
                kind: EntryKind::File,
                size: 0,
                mode: 0,
                modified: None,
                selected: false,
            },
            rank,
        )
    }
    let best = MatchRank {
        tier: 0,
        penalty: 0,
    };
    let worse = MatchRank {
        tier: 1,
        penalty: 0,
    };
    let mut hits = [
        hit("beta", "/root/beta", best),
        hit("Alpha", "/z/Alpha", best),
        hit("alpha", "/a/alpha", best),
        hit("aardvark", "/root/aardvark", worse),
    ];
    hits.sort();
    let paths: Vec<_> = hits.iter().map(|hit| hit.entry.path.as_path()).collect();
    assert_eq!(
        paths,
        [
            Path::new("/z/Alpha"),
            Path::new("/a/alpha"),
            Path::new("/root/beta"),
            Path::new("/root/aardvark")
        ]
    );
}

#[test]
fn mixed_utf8_and_non_utf8_names_obey_total_order_laws() {
    fn hit(bytes: &[u8], suffix: &str) -> SearchHit {
        let basename = OsString::from_vec(bytes.to_vec());
        SearchHit::new(
            FileEntry {
                path: PathBuf::from("/root").join(suffix).join(&basename),
                name: basename.to_string_lossy().into_owned(),
                kind: EntryKind::File,
                size: 0,
                mode: 0,
                modified: None,
                selected: false,
            },
            MatchRank {
                tier: 0,
                penalty: 0,
            },
        )
    }

    let z = hit(b"Z", "z");
    let a = hit(b"a", "a");
    let invalid = hit(&[0x60, 0x80], "invalid");
    let concrete = [&z, &a, &invalid];
    for left in concrete {
        for right in concrete {
            assert_eq!(left.cmp(right), right.cmp(left).reverse());
        }
    }
    for left in concrete {
        for middle in concrete {
            for right in concrete {
                if left <= middle && middle <= right {
                    assert!(left <= right, "comparison is not transitive");
                }
            }
        }
    }

    let cases = [
        hit(b"Alpha", "0"),
        hit(b"alpha", "1"),
        hit("Straße".as_bytes(), "2"),
        hit(b"STRASSE", "3"),
        hit("İ".as_bytes(), "4"),
        hit("i̇".as_bytes(), "5"),
        hit(&[0x60, 0x80], "6"),
        hit(&[0x61, 0xff], "7"),
        hit(&[0xfe], "8"),
    ];
    for left in &cases {
        assert_eq!(left.cmp(left), Ordering::Equal);
        for right in &cases {
            assert_eq!(left.cmp(right), right.cmp(left).reverse());
            assert_eq!(left == right, left.cmp(right) == Ordering::Equal);
            for third in &cases {
                if left <= right && right <= third {
                    assert!(left <= third, "comparison is not transitive");
                }
            }
        }
    }
}
