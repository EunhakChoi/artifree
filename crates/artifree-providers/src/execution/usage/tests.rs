//! Behavioral tests for the attempt group's usage counters (decisions 0042 rule 6 and 0056 rule
//! 14). The measured texts parse to their numbers, a missing, repeated or malformed value is
//! refused, and an absent peak file reads as `None`, while a peak file that exists but cannot be
//! read is refused.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{
    GroupUsage, UsageTexts, UsageUnreadable, keyed_value, read_usage, single_number,
    usage_from_texts,
};

/// The sequence number of the next temporary group directory of this process.
static NEXT_GROUP: AtomicU64 = AtomicU64::new(0);

// The texts that the measurement on the runner printed for a group that hit its limits.
const MEMORY_EVENTS_TEXT: &str = "low 0\nhigh 0\nmax 87\noom 1\noom_kill 4\noom_group_kill 1\n";
const PIDS_EVENTS_TEXT: &str = "max 1\n";
const CPU_STAT_TEXT: &str = "usage_usec 3088678\nuser_usec 3080000\nsystem_usec 8678\n\
    nr_periods 30\nnr_throttled 30\nthrottled_usec 2900000\nnr_bursts 0\nburst_usec 0\n";
const MEMORY_PEAK_TEXT: &str = "1073741824\n";
const PIDS_PEAK_TEXT: &str = "64\n";

/// What the evidence prints for the measured counters.
const MEASURED_TEXT: &str = concat!(
    "attempt_usage.memory_peak_bytes=1073741824\n",
    "attempt_usage.oom_kills=4\n",
    "attempt_usage.oom_group_kills=1\n",
    "attempt_usage.task_refusals=1\n",
    "attempt_usage.tasks_peak=64\n",
    "attempt_usage.cpu_usage_usec=3088678\n",
    "attempt_usage.cpu_periods=30\n",
    "attempt_usage.cpu_throttled_periods=30\n",
);

/// What the evidence prints for the measured counters when both peak files are absent.
const ABSENT_PEAKS_TEXT: &str = concat!(
    "attempt_usage.memory_peak_bytes=unavailable\n",
    "attempt_usage.oom_kills=4\n",
    "attempt_usage.oom_group_kills=1\n",
    "attempt_usage.task_refusals=1\n",
    "attempt_usage.tasks_peak=unavailable\n",
    "attempt_usage.cpu_usage_usec=3088678\n",
    "attempt_usage.cpu_periods=30\n",
    "attempt_usage.cpu_throttled_periods=30\n",
);

/// The texts of one group, owned so that a case can replace one of them.
#[derive(Clone, Debug)]
struct OwnedTexts {
    memory_events: String,
    pids_events: String,
    cpu_stat: String,
    memory_peak: Option<String>,
    pids_peak: Option<String>,
}

impl OwnedTexts {
    /// The texts that the measurement printed, with both peak files present.
    fn measured() -> Self {
        Self {
            memory_events: MEMORY_EVENTS_TEXT.to_owned(),
            pids_events: PIDS_EVENTS_TEXT.to_owned(),
            cpu_stat: CPU_STAT_TEXT.to_owned(),
            memory_peak: Some(MEMORY_PEAK_TEXT.to_owned()),
            pids_peak: Some(PIDS_PEAK_TEXT.to_owned()),
        }
    }

    /// The same texts with both peak files absent, as a kernel without them would show.
    fn without_peaks(mut self) -> Self {
        self.memory_peak = None;
        self.pids_peak = None;
        self
    }

    /// The borrowed view that the parser takes.
    fn borrowed(&self) -> UsageTexts<'_> {
        UsageTexts {
            memory_events: &self.memory_events,
            pids_events: &self.pids_events,
            cpu_stat: &self.cpu_stat,
            memory_peak: self.memory_peak.as_deref(),
            pids_peak: self.pids_peak.as_deref(),
        }
    }
}

/// The counters that `OwnedTexts::measured` holds.
fn measured_usage() -> GroupUsage {
    GroupUsage {
        memory_peak_bytes: Some(1_073_741_824),
        oom_kills: 4,
        oom_group_kills: 1,
        task_refusals: 1,
        tasks_peak: Some(64),
        cpu_usage_usec: 3_088_678,
        cpu_periods: 30,
        cpu_throttled_periods: 30,
    }
}

/// The counters that `OwnedTexts::measured().without_peaks()` holds.
fn absent_peaks_usage() -> GroupUsage {
    GroupUsage {
        memory_peak_bytes: None,
        tasks_peak: None,
        ..measured_usage()
    }
}

/// A fresh directory that stands for one attempt group, removed when dropped.
struct ScratchGroup {
    path: PathBuf,
}

impl ScratchGroup {
    /// Creates an empty directory whose name no other group of this process uses.
    fn new() -> io::Result<Self> {
        let sequence = NEXT_GROUP.fetch_add(1, Ordering::Relaxed);
        let name = format!("artifree-usage-tests-{}-{sequence}", process::id());
        let path = env::temp_dir().join(name);
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    /// The directory that stands for the group.
    fn path(&self) -> &Path {
        &self.path
    }

    /// Writes the usage files that `texts` holds. A peak that is `None` leaves its file absent.
    fn write(&self, texts: &OwnedTexts) -> io::Result<()> {
        fs::write(self.path.join("memory.events"), &texts.memory_events)?;
        fs::write(self.path.join("pids.events"), &texts.pids_events)?;
        fs::write(self.path.join("cpu.stat"), &texts.cpu_stat)?;
        if let Some(peak) = &texts.memory_peak {
            fs::write(self.path.join("memory.peak"), peak)?;
        }
        if let Some(peak) = &texts.pids_peak {
            fs::write(self.path.join("pids.peak"), peak)?;
        }
        Ok(())
    }
}

impl Drop for ScratchGroup {
    fn drop(&mut self) {
        // A directory that cannot be removed keeps its unique name, so no other test reads it.
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn measured_counters_parse() {
    let texts = OwnedTexts::measured();
    assert_eq!(
        usage_from_texts(&texts.borrowed()),
        Ok(measured_usage()),
        "the measured texts must parse to the measured counters",
    );
}

#[test]
fn keyed_value_matches_whole_keys_only() {
    assert_eq!(
        keyed_value("max 3\nmax.imposed 7\n", "max"),
        Ok(3),
        "the key max must not match the line max.imposed",
    );
    assert_eq!(
        keyed_value("xoom_kill 5\noom_kill 2\n", "oom_kill"),
        Ok(2),
        "the key oom_kill must not match the line xoom_kill",
    );
    assert_eq!(
        keyed_value("oom 1\n", "oom_kill"),
        Err(UsageUnreadable::Missing("oom_kill")),
        "the line oom must not satisfy the key oom_kill",
    );
}

/// One malformed-counter case: a name, an edit of the measured texts and the refusal it causes.
type MalformedCase = (&'static str, fn(&mut OwnedTexts), UsageUnreadable);

#[test]
fn malformed_counters_are_refused() {
    // Each case edits the measured texts once and names the refusal that the edit must cause.
    let cases: &[MalformedCase] = &[
        (
            "cpu.stat without nr_throttled",
            |texts: &mut OwnedTexts| {
                texts.cpu_stat = "usage_usec 3088678\nnr_periods 30\n".to_owned();
            },
            UsageUnreadable::Missing("nr_throttled"),
        ),
        (
            "memory.events without oom_group_kill",
            |texts: &mut OwnedTexts| texts.memory_events = "oom_kill 4\n".to_owned(),
            UsageUnreadable::Missing("oom_group_kill"),
        ),
        (
            "memory.events with oom_kill twice",
            |texts: &mut OwnedTexts| {
                texts.memory_events = "oom_kill 4\noom_kill 4\noom_group_kill 1\n".to_owned();
            },
            UsageUnreadable::Repeated("oom_kill"),
        ),
        (
            "pids.events with a max that is not a number",
            |texts: &mut OwnedTexts| texts.pids_events = "max many\n".to_owned(),
            UsageUnreadable::Malformed("max"),
        ),
        (
            "usage_usec with a trailing letter",
            |texts: &mut OwnedTexts| {
                texts.cpu_stat = "usage_usec 12x\nnr_periods 30\nnr_throttled 30\n".to_owned();
            },
            UsageUnreadable::Malformed("usage_usec"),
        ),
        (
            "usage_usec with no value",
            |texts: &mut OwnedTexts| {
                texts.cpu_stat = "usage_usec \nnr_periods 30\nnr_throttled 30\n".to_owned();
            },
            UsageUnreadable::Malformed("usage_usec"),
        ),
        (
            "usage_usec with a sign",
            |texts: &mut OwnedTexts| {
                texts.cpu_stat = "usage_usec -1\nnr_periods 30\nnr_throttled 30\n".to_owned();
            },
            UsageUnreadable::Malformed("usage_usec"),
        ),
        (
            "memory.peak that is max",
            |texts: &mut OwnedTexts| texts.memory_peak = Some("max\n".to_owned()),
            UsageUnreadable::Malformed("memory.peak"),
        ),
        (
            "memory.peak beyond u64",
            |texts: &mut OwnedTexts| texts.memory_peak = Some("18446744073709551616\n".to_owned()),
            UsageUnreadable::Malformed("memory.peak"),
        ),
        (
            "pids.peak with two lines",
            |texts: &mut OwnedTexts| texts.pids_peak = Some("64\n64\n".to_owned()),
            UsageUnreadable::Malformed("pids.peak"),
        ),
    ];
    for (name, edit, expected) in cases {
        let mut texts = OwnedTexts::measured();
        edit(&mut texts);
        assert_eq!(
            usage_from_texts(&texts.borrowed()),
            Err(*expected),
            "case `{name}` must be refused with {expected:?}",
        );
    }
}

#[test]
fn peak_files_hold_one_number_with_at_most_one_newline() {
    assert_eq!(
        single_number("1073741824\n", "memory.peak"),
        Ok(1_073_741_824),
        "a peak with one trailing newline is its number",
    );
    assert_eq!(
        single_number("64", "pids.peak"),
        Ok(64),
        "a peak without a trailing newline is its number",
    );
    assert_eq!(
        single_number("64\n\n", "pids.peak"),
        Err(UsageUnreadable::Malformed("pids.peak")),
        "a second newline makes a peak malformed",
    );
    assert_eq!(
        single_number("", "pids.peak"),
        Err(UsageUnreadable::Malformed("pids.peak")),
        "an empty peak is malformed",
    );
}

#[test]
fn absent_peaks_are_unavailable() {
    let texts = OwnedTexts::measured().without_peaks();
    assert_eq!(
        usage_from_texts(&texts.borrowed()),
        Ok(absent_peaks_usage()),
        "absent peak files must parse to None while the other counters stay",
    );
}

#[test]
fn display_prints_each_counter_on_its_own_line() {
    let measured = OwnedTexts::measured();
    assert_eq!(
        usage_from_texts(&measured.borrowed()).map(|usage| usage.to_string()),
        Ok(MEASURED_TEXT.to_owned()),
        "the evidence must print one attempt_usage line per counter",
    );
    let without_peaks = measured.without_peaks();
    assert_eq!(
        usage_from_texts(&without_peaks.borrowed()).map(|usage| usage.to_string()),
        Ok(ABSENT_PEAKS_TEXT.to_owned()),
        "an absent peak must print as unavailable",
    );
}

#[test]
fn read_usage_reads_a_group_directory() -> io::Result<()> {
    let group = ScratchGroup::new()?;
    group.write(&OwnedTexts::measured())?;
    assert_eq!(
        read_usage(group.path()),
        Ok(measured_usage()),
        "the group's files must read as the measured counters",
    );
    Ok(())
}

#[test]
fn read_usage_takes_absent_peak_files_as_unavailable() -> io::Result<()> {
    let group = ScratchGroup::new()?;
    group.write(&OwnedTexts::measured().without_peaks())?;
    assert_eq!(
        read_usage(group.path()),
        Ok(absent_peaks_usage()),
        "a group without its peak files must read with both peaks unavailable",
    );
    Ok(())
}

#[test]
fn read_usage_refuses_a_missing_required_file() -> io::Result<()> {
    let group = ScratchGroup::new()?;
    group.write(&OwnedTexts::measured())?;
    fs::remove_file(group.path().join("cpu.stat"))?;
    assert_eq!(
        read_usage(group.path()),
        Err(UsageUnreadable::File("cpu.stat")),
        "a group without cpu.stat must be refused by that file's name",
    );
    Ok(())
}

#[test]
fn read_usage_refuses_a_peak_file_that_exists_but_cannot_be_read() -> io::Result<()> {
    let group = ScratchGroup::new()?;
    group.write(&OwnedTexts::measured())?;
    fs::remove_file(group.path().join("memory.peak"))?;
    fs::create_dir(group.path().join("memory.peak"))?;
    assert_eq!(
        read_usage(group.path()),
        Err(UsageUnreadable::File("memory.peak")),
        "a peak that exists but cannot be read must not be taken as absent",
    );
    Ok(())
}
