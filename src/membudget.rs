//! A predictive memory-safety check for output-rendering steps whose memory
//! footprint can exceed the size of their input (currently: `--tree`). Before
//! committing to a potentially large allocation, estimate how much memory it
//! would need, compare that against the system's available memory, and refuse
//! — with a clear, actionable error — rather than let the allocator (and
//! potentially the OS OOM killer) decide.
//!
//! This exists because of a real incident during development: an early fix for
//! the tree renderer's unbounded recursion was validated with a deliberately
//! huge synthetic input and OOM-killed the development host (see SECURITY.md).
//! A depth cap closed that specific hole; this is a second, more general layer
//! — predict, check, refuse cleanly — rather than relying solely on a
//! structural cap to keep memory use small.

use std::fs;

/// Parsed system memory info, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// Outcome of a budget check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckResult {
    /// Estimated usage leaves at least `min_free_pct` of total memory free.
    Proceed,
    /// System memory could not be determined (non-Linux, sandboxed, or
    /// `/proc/meminfo` unreadable/unparseable). The caller should warn and
    /// proceed — a missing safety net isn't a reason to refuse to run.
    Unknown,
    /// Proceeding would leave less than `min_free_pct` of total memory free.
    Refuse(BudgetError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "refusing: this would need an estimated ~{estimated_mb} MB, which would leave less than \
     {min_free_pct}% of this system's {total_mb} MB free (~{available_mb} MB is currently \
     available); simplify the request, or pass --min-free-mem-pct to change the safety margin"
)]
pub struct BudgetError {
    pub estimated_mb: u64,
    pub available_mb: u64,
    pub total_mb: u64,
    pub min_free_pct: u8,
}

const BYTES_PER_KIB: u64 = 1024;
const BYTES_PER_MIB: u64 = 1024 * 1024;

/// Check `estimated_bytes` against the system's currently available memory,
/// refusing if proceeding would leave less than `min_free_pct` of *total*
/// system memory free.
pub fn check(estimated_bytes: u64, min_free_pct: u8) -> CheckResult {
    match read_meminfo() {
        Some(mem) => decide(mem, estimated_bytes, min_free_pct),
        None => CheckResult::Unknown,
    }
}

/// The pure decision: given memory info, an estimate, and a margin, decide
/// whether to proceed. Separated from `check` so it's testable with synthetic
/// numbers, with no real system call involved.
fn decide(mem: MemInfo, estimated_bytes: u64, min_free_pct: u8) -> CheckResult {
    let min_free_bytes = mem.total_bytes.saturating_mul(min_free_pct as u64) / 100;
    let usable_bytes = mem.available_bytes.saturating_sub(min_free_bytes);

    if estimated_bytes > usable_bytes {
        CheckResult::Refuse(BudgetError {
            estimated_mb: estimated_bytes / BYTES_PER_MIB,
            available_mb: mem.available_bytes / BYTES_PER_MIB,
            total_mb: mem.total_bytes / BYTES_PER_MIB,
            min_free_pct,
        })
    } else {
        CheckResult::Proceed
    }
}

/// Read system memory info. Linux: `/proc/meminfo`. Overridable via
/// `RGQ_MEM_AVAILABLE_BYTES` / `RGQ_MEM_TOTAL_BYTES` (both must be set) for
/// deterministic testing, and as an escape hatch on systems where
/// `/proc/meminfo` isn't available or doesn't reflect a real constraint.
///
/// Known limitation: this reads *host*-level memory. Inside a memory-limited
/// container (e.g. `docker run --memory=512m`), `/proc/meminfo` still reports
/// the host's full memory, not the cgroup limit — this check would not catch
/// an OOM kill imposed by a tighter cgroup limit than the host's total RAM.
fn read_meminfo() -> Option<MemInfo> {
    if let (Ok(avail), Ok(total)) = (
        std::env::var("RGQ_MEM_AVAILABLE_BYTES"),
        std::env::var("RGQ_MEM_TOTAL_BYTES"),
    ) {
        if let (Ok(available_bytes), Ok(total_bytes)) = (avail.parse(), total.parse()) {
            return Some(MemInfo {
                total_bytes,
                available_bytes,
            });
        }
    }
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo(&content)
}

/// Parse `/proc/meminfo` content, extracting `MemTotal` and `MemAvailable`
/// (both reported in KiB by kernel convention, regardless of the literal "kB"
/// label). `None` if either is missing or malformed — never partial.
fn parse_meminfo(content: &str) -> Option<MemInfo> {
    let mut total_kib = None;
    let mut available_kib = None;

    for line in content.lines() {
        let Some((label, rest)) = line.split_once(':') else {
            continue;
        };
        let value_kib = rest
            .split_whitespace()
            .next()
            .and_then(|tok| tok.parse::<u64>().ok());
        match label.trim() {
            "MemTotal" => total_kib = value_kib,
            "MemAvailable" => available_kib = value_kib,
            _ => {}
        }
    }

    Some(MemInfo {
        total_bytes: total_kib? * BYTES_PER_KIB,
        available_bytes: available_kib? * BYTES_PER_KIB,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MEMINFO: &str = "\
MemTotal:       32898000 kB
MemFree:         1234567 kB
MemAvailable:   28381234 kB
Buffers:          123456 kB
Cached:          2345678 kB
SwapTotal:      41943040 kB
SwapFree:       32000000 kB
";

    #[test]
    fn parses_real_world_meminfo_format() {
        let mem = parse_meminfo(SAMPLE_MEMINFO).expect("should parse");
        assert_eq!(mem.total_bytes, 32_898_000 * BYTES_PER_KIB);
        assert_eq!(mem.available_bytes, 28_381_234 * BYTES_PER_KIB);
    }

    #[test]
    fn missing_mem_available_is_none() {
        let content = "MemTotal:       32898000 kB\nMemFree: 1234 kB\n";
        assert!(parse_meminfo(content).is_none());
    }

    #[test]
    fn malformed_or_empty_content_is_none() {
        assert!(parse_meminfo("not meminfo at all").is_none());
        assert!(parse_meminfo("").is_none());
        // A line without ':' must be skipped, not abort parsing of the rest.
        assert!(parse_meminfo("garbage line\nMemTotal: 100 kB\nMemAvailable: 50 kB\n").is_some());
    }

    #[test]
    fn decide_proceeds_with_ample_headroom() {
        let mem = MemInfo {
            total_bytes: 1_000_000,
            available_bytes: 900_000,
        };
        assert_eq!(decide(mem, 1_000, 20), CheckResult::Proceed);
    }

    #[test]
    fn decide_refuses_when_it_would_dip_below_the_margin() {
        let mem = MemInfo {
            total_bytes: 1_000_000,
            available_bytes: 900_000,
        };
        // min_free = 200_000, usable = 700_000; estimate of 800_000 exceeds it.
        assert!(matches!(decide(mem, 800_000, 20), CheckResult::Refuse(_)));
    }

    #[test]
    fn decide_boundary_is_inclusive_of_exactly_filling_the_budget() {
        let mem = MemInfo {
            total_bytes: 1_000_000,
            available_bytes: 900_000,
        };
        // usable = 900_000 - 200_000 = 700_000 exactly.
        assert_eq!(
            decide(mem, 700_000, 20),
            CheckResult::Proceed,
            "using exactly the budget must be allowed"
        );
        assert!(
            matches!(decide(mem, 700_001, 20), CheckResult::Refuse(_)),
            "one byte over must refuse"
        );
    }

    #[test]
    fn already_below_margin_refuses_even_a_tiny_estimate() {
        // Already < 20% of total free, before rgq does anything at all.
        let mem = MemInfo {
            total_bytes: 1_000_000,
            available_bytes: 100_000,
        };
        assert!(matches!(decide(mem, 1, 20), CheckResult::Refuse(_)));
    }

    #[test]
    fn zero_pct_margin_only_refuses_past_currently_available() {
        let mem = MemInfo {
            total_bytes: 1_000_000,
            available_bytes: 500_000,
        };
        assert_eq!(decide(mem, 500_000, 0), CheckResult::Proceed);
        assert!(matches!(decide(mem, 500_001, 0), CheckResult::Refuse(_)));
    }

    #[test]
    fn error_message_names_the_override_flag() {
        let mem = MemInfo {
            total_bytes: 1_000_000,
            available_bytes: 900_000,
        };
        let CheckResult::Refuse(err) = decide(mem, 800_000, 20) else {
            panic!("expected a refusal");
        };
        assert!(err.to_string().contains("--min-free-mem-pct"));
    }
}
