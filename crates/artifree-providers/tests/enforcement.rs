//! Linux enforcement checks for host admission (decision 0056 rules 9, 10, 12 and 13).
//!
//! `contributor_host_runs_no_payload_and_reports_a_refusal` runs on every Linux host. It
//! reads only the core routing policy and starts no process. The `job_*` tests change the
//! host, so they run only in the `Linux enforcement` job: one at a time, each by its exact
//! name, and each asserts the refusal that names the condition its job step prepared or
//! removed. No test returns early, so a missing precondition fails instead of passing.

/// The core routing file that the admission reads.
#[cfg(target_os = "linux")]
const CORE_PATTERN: &str = "/proc/sys/kernel/core_pattern";

/// Asserts that `inspect_host` refuses this host with `expected` and carries no report.
#[cfg(target_os = "linux")]
fn assert_refused_with(expected: artifree_providers::execution::AdmissionRefusal) {
    let outcome = artifree_providers::execution::inspect_host(
        artifree_providers::execution::InspectionProfile::fixture(),
    );
    assert_eq!(
        outcome.refusal(),
        expected,
        "the facility that names this condition must refuse the host"
    );
    assert!(
        outcome.report().is_none(),
        "a refused host must not carry an evidence report"
    );
}

/// Returns the value of the one `key=value` line of `text` whose key is `key`.
#[cfg(target_os = "linux")]
fn report_value<'a>(text: &'a str, key: &str) -> Result<&'a str, String> {
    let prefix = format!("{key}=");
    let mut values = text
        .lines()
        .filter_map(|line| line.strip_prefix(prefix.as_str()));
    let value = values
        .next()
        .ok_or_else(|| format!("the report has no `{key}` line"))?;
    if values.next().is_some() {
        return Err(format!("the report repeats the `{key}` line"));
    }
    Ok(value)
}

/// Fails with `message` unless `condition` holds.
#[cfg(target_os = "linux")]
fn require(condition: bool, message: String) -> Result<(), String> {
    if condition { Ok(()) } else { Err(message) }
}

/// Whether `text` is a lowercase hexadecimal SHA-256 digest.
#[cfg(target_os = "linux")]
fn is_sha256_hex(text: &str) -> bool {
    let is_hex = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    text.len() == 64 && text.bytes().all(is_hex)
}

/// Fails unless the report line `key` holds exactly `expected`.
#[cfg(target_os = "linux")]
fn expect_report_value(text: &str, key: &str, expected: &str) -> Result<(), String> {
    let actual = report_value(text, key)?;
    require(
        actual == expected,
        format!("report line `{key}` is `{actual}`, expected `{expected}`"),
    )
}

/// Checks the evidence that an admission run records (decision 0056 rule 13).
#[cfg(target_os = "linux")]
fn check_report_rule_13(text: &str) -> Result<(), String> {
    for (key, expected) in [
        ("limit.memory.max", "1073741824"),
        ("limit.memory.swap.max", "0"),
        ("limit.memory.oom.group", "1"),
        ("limit.cpu.max", "100000 100000"),
        ("limit.pids.max", "64"),
        ("listener_counters", "not measured in this slice"),
        ("architecture", std::env::consts::ARCH),
    ] {
        expect_report_value(text, key, expected)?;
    }
    let controllers = report_value(text, "delegated_controllers")?;
    let tokens: Vec<&str> = controllers.split_whitespace().collect();
    for required in ["memory", "pids", "cpu"] {
        require(
            tokens.contains(&required),
            format!("delegated_controllers `{controllers}` lacks `{required}`"),
        )?;
    }
    require(
        report_value(text, "tool_identity")?.starts_with("unverified"),
        "the report must not claim a verified tool identity".to_owned(),
    )?;
    for key in ["launcher_sha256", "policy_sha256"] {
        let digest = report_value(text, key)?;
        require(
            is_sha256_hex(digest),
            format!("`{key}` is not a SHA-256 digest: `{digest}`"),
        )?;
    }
    require(
        !report_value(text, "kernel_release")?.is_empty(),
        "the report must record the kernel release".to_owned(),
    )?;
    let launcher = report_value(text, "launcher_version")?;
    require(
        !launcher.is_empty() && launcher != "unavailable",
        format!("the report must record the launcher version, got `{launcher}`"),
    )?;
    let routing = std::fs::read_to_string(CORE_PATTERN)
        .map_err(|error| format!("cannot read {CORE_PATTERN}: {error}"))?;
    expect_report_value(text, "core_routing", routing.trim_end())?;
    let peak = report_value(text, "peak_memory_bytes")?;
    require(
        peak == "unavailable" || peak.parse::<u64>().is_ok(),
        format!("peak_memory_bytes must be a byte count or `unavailable`, got `{peak}`"),
    )?;
    let cleanup = report_value(text, "cleanup_observation")?;
    require(
        cleanup.contains("populated 0"),
        format!("the cleanup observation must record an empty group, got `{cleanup}`"),
    )?;
    require(
        !text.contains("supported"),
        format!("the report must not call the host supported:\n{text}"),
    )
}

/// Runs on every Linux host: the routing read and its re-read are the only checks here.
#[cfg(target_os = "linux")]
#[test]
fn contributor_host_runs_no_payload_and_reports_a_refusal() {
    // Neither call starts a process. The admission, the launcher and the probe belong to
    // `inspect_host`, which this test never calls.
    let profile = artifree_providers::execution::InspectionProfile::fixture();
    match artifree_providers::execution::RoutingObservation::inspect(profile) {
        Ok(observation) => {
            let outcome = observation.refuse_execution();
            assert!(
                outcome.is_err(),
                "refuse_execution must never permit execution, got {outcome:?}"
            );
        }
        Err(refusal) => assert!(
            matches!(
                refusal,
                artifree_providers::execution::AdmissionRefusal::UnreadableCoreRouting
                    | artifree_providers::execution::AdmissionRefusal::UnknownCoreRouting
                    | artifree_providers::execution::AdmissionRefusal::PipeCoreRouting
                    | artifree_providers::execution::AdmissionRefusal::SocketCoreRouting
            ),
            "the routing read may refuse only on routing, got {refusal}"
        ),
    }
}

/// Runs on hosts without Linux: the operating system refusal comes before any read.
#[cfg(not(target_os = "linux"))]
#[test]
fn non_linux_host_is_refused_before_any_read() {
    let outcome = artifree_providers::execution::inspect_host(
        artifree_providers::execution::InspectionProfile::fixture(),
    );
    assert_eq!(
        outcome.refusal(),
        artifree_providers::execution::AdmissionRefusal::UnsupportedOperatingSystem,
        "a host without Linux must be refused as an unsupported operating system"
    );
    assert!(
        outcome.report().is_none(),
        "a refused host must not carry an evidence report"
    );
}

/// The host as found, with cores routed to a pipe and nothing prepared.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "runs only in the Linux enforcement job (decision 0056 rule 12)"]
fn job_unprepared_host_is_refused() {
    assert_refused_with(artifree_providers::execution::AdmissionRefusal::PipeCoreRouting);
}

/// Rule 10: the job routes cores to a pipe again.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "runs only in the Linux enforcement job (decision 0056 rule 12)"]
fn job_pipe_routing_is_refused() {
    assert_refused_with(artifree_providers::execution::AdmissionRefusal::PipeCoreRouting);
}

/// Rule 10: the launcher's profile is removed, so the namespaces cannot be made.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "runs only in the Linux enforcement job (decision 0056 rule 12)"]
fn job_missing_profile_is_refused() {
    assert_refused_with(artifree_providers::execution::AdmissionRefusal::NamespacesUnavailable);
}

/// Rule 10: the delegated group lacks the memory controller.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "runs only in the Linux enforcement job (decision 0056 rule 12)"]
fn job_missing_controller_is_refused() {
    assert_refused_with(
        artifree_providers::execution::AdmissionRefusal::MissingController(
            artifree_providers::execution::Controller::Memory,
        ),
    );
}

/// Rule 10: the unit's group is not delegated to the runner's account.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "runs only in the Linux enforcement job (decision 0056 rule 12)"]
fn job_undelegated_group_is_refused() {
    assert_refused_with(artifree_providers::execution::AdmissionRefusal::UndelegatedCgroup);
}

/// Rule 10: another process shares the delegated group.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "runs only in the Linux enforcement job (decision 0056 rule 12)"]
fn job_occupied_group_is_refused() {
    assert_refused_with(artifree_providers::execution::AdmissionRefusal::OccupiedCgroup);
}

/// Rule 10: the tool root's launcher is moved away.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "runs only in the Linux enforcement job (decision 0056 rule 12)"]
fn job_missing_launcher_is_refused() {
    assert_refused_with(artifree_providers::execution::AdmissionRefusal::MissingLauncher);
}

/// Rule 10: the tool root's fixture is moved away.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "runs only in the Linux enforcement job (decision 0056 rule 12)"]
fn job_missing_fixture_is_refused() {
    assert_refused_with(artifree_providers::execution::AdmissionRefusal::MissingFixture);
}

/// Rule 9, part 3: the prepared host passes every implemented facility and then refuses.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "runs only in the Linux enforcement job (decision 0056 rule 12)"]
fn job_prepared_host_passes_every_facility() -> Result<(), Box<dyn std::error::Error>> {
    let profile = artifree_providers::execution::InspectionProfile::fixture();
    let outcome = artifree_providers::execution::inspect_host(profile);
    assert_eq!(
        outcome.refusal(),
        artifree_providers::execution::AdmissionRefusal::IncompleteEnforcement,
        "a prepared host must pass every implemented facility and stop at rule 16"
    );
    let report = outcome
        .report()
        .ok_or("a prepared host must carry the evidence report")?;
    let text = report.to_string();
    let path = std::env::var_os("ARTIFREE_ENFORCEMENT_REPORT")
        .ok_or("ARTIFREE_ENFORCEMENT_REPORT must name the file that receives the report")?;
    // Written before the checks, so the evidence survives a failed check.
    std::fs::write(path, text.as_bytes())?;
    check_report_rule_13(&text)?;
    let observation = artifree_providers::execution::RoutingObservation::inspect(profile)?;
    assert_eq!(
        observation.refuse_execution(),
        Err(artifree_providers::execution::AdmissionRefusal::IncompleteEnforcement),
        "the routing re-check must also refuse the prepared host"
    );
    Ok(())
}
