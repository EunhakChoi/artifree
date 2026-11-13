//! Behavioral tests for the `run_profile` vocabulary of the domain (decision
//! 0057): the profile name grammar, the attempt identity text form, and the
//! closed words of revocations, attempt states, and checkpoint stop reasons.
//!
//! A stored word must parse back to the same value, a word outside a closed
//! vocabulary must stay refused, and each revocation must answer and settle
//! the way decision 0057 rules 7 and 10 say.

use std::fmt;
use std::str::FromStr;

use artifree_domain::identity::InvalidValue;
use artifree_domain::job::{
    AttemptIdentity, AttemptState, JobFailure, JobKind, ProfileName, Revocation, StopReason,
};

/// The bytes 0 to 15, and their lowercase hexadecimal text.
const IDENTITY_BYTES: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const IDENTITY_HEX: &str = "000102030405060708090a0b0c0d0e0f";

/// Every stored revocation word, with the revocation it names.
const REVOCATIONS: [(Revocation, &str); 7] = [
    (Revocation::Cancel, "cancel"),
    (Revocation::Trash, "trash"),
    (Revocation::Purge, "purge"),
    (Revocation::Generation, "generation"),
    (Revocation::Authority, "authority"),
    (Revocation::Lease, "lease"),
    (Revocation::Shutdown, "shutdown"),
];

/// Every stored attempt state word, with the state it names.
const ATTEMPT_STATES: [(AttemptState, &str); 3] = [
    (AttemptState::Live, "live"),
    (AttemptState::CleanupPending, "cleanup_pending"),
    (AttemptState::Settled, "settled"),
];

/// Every checkpoint stop reason with its word. `fence` stores no revocation.
const STOP_REASONS: [(StopReason, &str); 7] = [
    (StopReason::Cancel, "cancel"),
    (StopReason::Trash, "trash"),
    (StopReason::Purge, "purge"),
    (StopReason::Generation, "generation"),
    (StopReason::Authority, "authority"),
    (StopReason::Lease, "lease"),
    (StopReason::Fence, "fence"),
];

/// The revocation each stored revocation is answered with at a checkpoint.
/// `shutdown` is answered as `lease` (rule 7 has no shutdown answer).
const REVOCATION_ANSWERS: [(Revocation, StopReason); 7] = [
    (Revocation::Cancel, StopReason::Cancel),
    (Revocation::Trash, StopReason::Trash),
    (Revocation::Purge, StopReason::Purge),
    (Revocation::Generation, StopReason::Generation),
    (Revocation::Authority, StopReason::Authority),
    (Revocation::Lease, StopReason::Lease),
    (Revocation::Shutdown, StopReason::Lease),
];

/// Which revocations settle their job as `cancelled` (rule 10). `lease` and
/// `shutdown` end as `retry_scheduled` or `failed` by their attempts instead.
const REVOCATION_ENDS_CANCELLED: [(Revocation, bool); 7] = [
    (Revocation::Cancel, true),
    (Revocation::Trash, true),
    (Revocation::Purge, true),
    (Revocation::Generation, true),
    (Revocation::Authority, true),
    (Revocation::Lease, false),
    (Revocation::Shutdown, false),
];

/// Checks that `variant` is stored as `word` and that `word` parses back to it.
fn assert_word<T>(variant: T, word: &str, as_str: fn(T) -> &'static str)
where
    T: FromStr<Err = InvalidValue> + Copy + fmt::Debug + PartialEq,
{
    assert_eq!(as_str(variant), word, "stored word of {variant:?}");
    assert_eq!(
        word.parse::<T>(),
        Ok(variant),
        "{word:?} must parse back to {variant:?}"
    );
}

/// Checks that `word` is refused by the parser of `T`, naming the kind of value.
fn assert_refused<T>(word: &str, what: &'static str)
where
    T: FromStr<Err = InvalidValue>,
{
    assert_eq!(
        word.parse::<T>().err().map(|error| error.what),
        Some(what),
        "{word:?} must be refused as {what}"
    );
}

#[test]
fn profile_names_follow_their_grammar() {
    let longest = format!("a{}", "0".repeat(31));
    for name in ["fixture", "a", "a-1", longest.as_str()] {
        let parsed = name
            .parse::<ProfileName>()
            .map(|profile| profile.to_string());
        assert_eq!(
            parsed,
            Ok(name.to_owned()),
            "profile name {name:?} must be kept as written"
        );
    }

    let too_long = "a".repeat(33);
    for name in [
        "",
        "1abc",
        "-a",
        "Fixture",
        "a_b",
        "a b",
        "a\u{e9}",
        too_long.as_str(),
    ] {
        let result = name.parse::<ProfileName>();
        assert!(
            matches!(
                result,
                Err(InvalidValue {
                    what: "profile name",
                    ..
                })
            ),
            "{name:?} must be refused as a profile name, got {result:?}"
        );
    }
}

#[test]
fn attempt_identities_round_trip_as_lowercase_hex() -> Result<(), InvalidValue> {
    let identity = AttemptIdentity::from_random(IDENTITY_BYTES);
    assert_eq!(identity.hex(), IDENTITY_HEX);
    assert_eq!(identity.to_string(), IDENTITY_HEX);

    let parsed: AttemptIdentity = IDENTITY_HEX.parse()?;
    assert_eq!(parsed, identity);
    assert_eq!(parsed.bytes(), IDENTITY_BYTES);
    Ok(())
}

#[test]
fn attempt_identities_refuse_other_spellings() {
    let short = "0".repeat(31);
    let long = "0".repeat(33);
    let uppercase = format!("A{}", "0".repeat(31));
    let last_uppercase = format!("{}A", "0".repeat(31));
    let non_hex = format!("g{}", "0".repeat(31));
    for text in [
        short.as_str(),
        long.as_str(),
        uppercase.as_str(),
        last_uppercase.as_str(),
        non_hex.as_str(),
    ] {
        let result = text.parse::<AttemptIdentity>();
        assert!(
            matches!(
                result,
                Err(InvalidValue {
                    what: "attempt identity",
                    ..
                })
            ),
            "{text:?} must be refused as an attempt identity, got {result:?}"
        );
    }
}

#[test]
fn run_profile_words_round_trip() {
    assert_word(JobKind::RunProfile, "run_profile", JobKind::as_str);
    assert_word(JobKind::VerifyRevision, "verify_revision", JobKind::as_str);
    assert_word(
        JobFailure::ExecutionUnavailable,
        "execution_unavailable",
        JobFailure::as_str,
    );
    for (revocation, word) in REVOCATIONS {
        assert_word(revocation, word, Revocation::as_str);
    }
    for (state, word) in ATTEMPT_STATES {
        assert_word(state, word, AttemptState::as_str);
    }
    // `StopReason` has no parser: a checkpoint answer is only written, never read.
    for (reason, word) in STOP_REASONS {
        assert_eq!(reason.as_str(), word, "stored word of {reason:?}");
    }
}

#[test]
fn closed_vocabularies_refuse_other_words() {
    assert_refused::<JobKind>("run-profile", "job kind");
    assert_refused::<JobFailure>("execution-unavailable", "job failure");
    // `fence` is a checkpoint answer that stores no revocation, so it is not a
    // stored revocation word.
    assert_refused::<Revocation>("fence", "revocation");
    assert_refused::<AttemptState>("running", "attempt state");
}

#[test]
fn execution_unavailable_is_not_retryable() {
    assert!(
        !JobFailure::ExecutionUnavailable.is_retryable(),
        "an unknown profile cannot succeed on another attempt"
    );
    assert!(
        JobFailure::StorageUnavailable.is_retryable(),
        "storage failures stay retryable, so the retry check is not constant"
    );
}

#[test]
fn revocations_map_to_their_answers_and_settlements() {
    for (revocation, reason) in REVOCATION_ANSWERS {
        assert_eq!(
            revocation.stop_reason(),
            reason,
            "{revocation:?} must be answered as {reason:?}"
        );
    }
    for (revocation, cancelled) in REVOCATION_ENDS_CANCELLED {
        assert_eq!(
            revocation.ends_cancelled(),
            cancelled,
            "settling {revocation:?} must give cancelled = {cancelled}"
        );
    }

    assert_eq!(
        StopReason::Fence.revocation(),
        None,
        "a fence refusal stores no revocation"
    );
    assert_eq!(StopReason::Trash.revocation(), Some(Revocation::Trash));
    for reason in [
        StopReason::Cancel,
        StopReason::Trash,
        StopReason::Purge,
        StopReason::Generation,
        StopReason::Authority,
        StopReason::Lease,
    ] {
        assert_eq!(
            reason.revocation().map(Revocation::stop_reason),
            Some(reason),
            "{reason:?} must be answered the same way after it is stored"
        );
    }
}
