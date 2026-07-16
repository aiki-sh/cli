//! Detection and stripping of the macOS `com.apple.quarantine` extended attribute
//!
//! Browsers and most GUI apps stamp downloads with the quarantine xattr
//! (curl/wget/npm/cargo-install do not; Homebrew removes it, but Homebrew
//! casks deliver quarantined binaries). On the first exec of a quarantined
//! binary, syspolicyd blocks the exec — a silent multi-second delay, a GUI
//! dialog, or SIGKILL for unsigned binaries. Once the user approves,
//! macOS sets the approved bit in the flags field and later execs run clean.
//!
//! The xattr value looks like `0083;689ab12c;Chrome;UUID`: semicolon-separated
//! fields, the first a hex flags bitfield. Flag parsing is a pure function
//! over the raw bytes so it unit-tests on every platform; only the syscalls
//! are macOS-gated.

use std::io;
use std::path::Path;

/// Bit set in the flags field once the user has approved the binary in the
/// Gatekeeper dialog.
const APPROVED_BIT: u32 = 0x0040;

#[cfg(target_os = "macos")]
const XATTR_NAME: &std::ffi::CStr = c"com.apple.quarantine";

/// Result of inspecting a path for the quarantine xattr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineStatus {
    /// Verified: no quarantine xattr present (or non-macOS build).
    NotQuarantined,
    /// Xattr present with the approved bit set: no dialog will fire.
    Approved,
    /// Xattr present, not yet approved: exec will block.
    Pending { raw: String },
    /// Xattr present but the value could not be interpreted.
    Undetermined { raw: String },
    /// getxattr failed for a reason other than ENOATTR — "could not inspect"
    /// is a different fact from "inspected and clean".
    CheckFailed { errno: i32 },
}

/// Check `path` for the quarantine xattr.
///
/// On macOS this is a single getxattr syscall that follows symlinks
/// (options=0), so a Homebrew-linked binary resolves to its Caskroom target.
/// On other platforms the xattr does not exist, so nothing is ever quarantined
/// and everything downstream dead-code-eliminates in release builds.
pub fn check(path: &Path) -> QuarantineStatus {
    // Test seam: debug builds honor a forced status so non-macOS test rigs
    // can drive the Pending/Undetermined/CheckFailed paths (and macOS tests
    // can force a status without stamping a real xattr). Compiled out of
    // release builds entirely.
    #[cfg(debug_assertions)]
    if let Some(forced) = test_forced_status(path) {
        return forced;
    }
    check_impl(path)
}

/// Test-only override for [`check`], read from `AIKI_TEST_QUARANTINE_STATUS`.
///
/// Value grammar: `pending|undetermined|checkfailed[:<path>]`. With the
/// `:<path>` suffix the forced status applies only to that exact path (a
/// later doctor task forces a status on one specific binary); without it,
/// every checked path reports the forced status. The seam is read-only — it
/// fakes `check()` and gives `strip()` nothing to mutate, which is why
/// mutation-asserting tests still need real xattr fixtures on macOS.
#[cfg(debug_assertions)]
fn test_forced_status(path: &Path) -> Option<QuarantineStatus> {
    let var = std::env::var("AIKI_TEST_QUARANTINE_STATUS").ok()?;
    let (status, scope) = match var.split_once(':') {
        Some((s, p)) => (s, Some(p.to_string())),
        None => (var.as_str(), None),
    };
    if let Some(scope) = scope {
        if Path::new(&scope) != path {
            return None;
        }
    }
    match status {
        "pending" => Some(QuarantineStatus::Pending {
            raw: "0083;0;aiki-test;forced".to_string(),
        }),
        "undetermined" => Some(QuarantineStatus::Undetermined {
            raw: "aiki-test-forced".to_string(),
        }),
        "checkfailed" => Some(QuarantineStatus::CheckFailed { errno: 5 }),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn check_impl(path: &Path) -> QuarantineStatus {
    let c_path = match std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
        Ok(p) => p,
        Err(_) => return QuarantineStatus::CheckFailed { errno: libc::EINVAL },
    };

    // Quarantine values are ~100 bytes; an oversized buffer keeps this to one
    // syscall instead of the size-probe-then-read dance.
    let mut buf = [0u8; 4096];
    let len = unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            XATTR_NAME.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
            0, // position: only meaningful for resource forks
            0, // options: 0 follows symlinks
        )
    };

    if len < 0 {
        let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
        return if errno == libc::ENOATTR {
            QuarantineStatus::NotQuarantined
        } else {
            QuarantineStatus::CheckFailed { errno }
        };
    }

    parse_quarantine_value(&buf[..len as usize])
}

#[cfg(not(target_os = "macos"))]
fn check_impl(_path: &Path) -> QuarantineStatus {
    QuarantineStatus::NotQuarantined
}

/// Remove the quarantine xattr from `path`. Follows symlinks (options=0).
#[cfg(target_os = "macos")]
pub fn strip(path: &Path) -> io::Result<()> {
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let rc = unsafe { libc::removexattr(c_path.as_ptr(), XATTR_NAME.as_ptr(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn strip(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Interpret a raw quarantine xattr value.
///
/// Well-formed values (UTF-8, semicolon-separated fields, first field valid
/// hex that fits in u32) map to [`QuarantineStatus::Approved`] or
/// [`QuarantineStatus::Pending`] depending on the approved bit; anything else
/// maps to [`QuarantineStatus::Undetermined`] with the lossy-decoded value.
fn parse_quarantine_value(bytes: &[u8]) -> QuarantineStatus {
    let raw = String::from_utf8_lossy(bytes).into_owned();

    let Ok(value) = std::str::from_utf8(bytes) else {
        return QuarantineStatus::Undetermined { raw };
    };

    let mut fields = value.split(';');
    let flags_field = fields.next().unwrap_or_default();
    if fields.next().is_none() {
        // No semicolon: covers the empty value and single-field strings.
        return QuarantineStatus::Undetermined { raw };
    }

    match u32::from_str_radix(flags_field, 16) {
        Ok(flags) if flags & APPROVED_BIT != 0 => QuarantineStatus::Approved,
        Ok(_) => QuarantineStatus::Pending { raw },
        Err(_) => QuarantineStatus::Undetermined { raw },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> QuarantineStatus {
        parse_quarantine_value(value.as_bytes())
    }

    #[test]
    fn pending_when_approved_bit_clear() {
        let value = "0083;689ab12c;Chrome;F5A19714-2C21-4E45-8189-A6B7E05C7E2E";
        assert_eq!(
            parse(value),
            QuarantineStatus::Pending { raw: value.to_string() }
        );
    }

    #[test]
    fn approved_when_approved_bit_set() {
        // 00c3 synthetic; 01c1/03c1 observed on real user-approved downloads.
        for value in [
            "00c3;689ab12c;Chrome;F5A19714-2C21-4E45-8189-A6B7E05C7E2E",
            "01c1;689ab12c;Safari;F5A19714-2C21-4E45-8189-A6B7E05C7E2E",
            "03c1;689ab12c;Safari;F5A19714-2C21-4E45-8189-A6B7E05C7E2E",
        ] {
            assert_eq!(parse(value), QuarantineStatus::Approved, "value: {value}");
        }
    }

    #[test]
    fn undetermined_on_empty_value() {
        assert_eq!(
            parse(""),
            QuarantineStatus::Undetermined { raw: String::new() }
        );
    }

    #[test]
    fn undetermined_on_missing_fields() {
        assert_eq!(
            parse("0083"),
            QuarantineStatus::Undetermined { raw: "0083".to_string() }
        );
    }

    #[test]
    fn undetermined_on_non_hex_flags() {
        let value = "friday;689ab12c;Chrome;UUID";
        assert_eq!(
            parse(value),
            QuarantineStatus::Undetermined { raw: value.to_string() }
        );
    }

    #[test]
    fn undetermined_on_non_utf8_bytes() {
        match parse_quarantine_value(b"\xff\xfe0083;689ab12c;Chrome;UUID") {
            QuarantineStatus::Undetermined { raw } => {
                assert!(raw.contains('\u{FFFD}'), "raw should be lossy-decoded: {raw}");
            }
            other => panic!("expected Undetermined, got {other:?}"),
        }
    }

    #[test]
    fn undetermined_on_flags_overflowing_u32() {
        let value = "1ffffffff;689ab12c;Chrome;UUID";
        assert_eq!(
            parse(value),
            QuarantineStatus::Undetermined { raw: value.to_string() }
        );
    }

    /// Single test for the path-scoped seam grammar: one test (not several)
    /// because the env var is process-global and lib tests run concurrently —
    /// the path scope keeps concurrent `check()` calls on other paths honest.
    #[cfg(debug_assertions)]
    #[test]
    fn forced_status_seam_is_path_scoped() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let other = tempfile::NamedTempFile::new().unwrap();
        let scoped = format!("pending:{}", file.path().display());
        std::env::set_var("AIKI_TEST_QUARANTINE_STATUS", &scoped);

        match check(file.path()) {
            QuarantineStatus::Pending { .. } => {}
            got => panic!("scoped path must report the forced status, got {got:?}"),
        }
        assert_eq!(
            check(other.path()),
            QuarantineStatus::NotQuarantined,
            "non-scoped path must fall through to the real check"
        );

        std::env::set_var(
            "AIKI_TEST_QUARANTINE_STATUS",
            format!("checkfailed:{}", file.path().display()),
        );
        match check(file.path()) {
            QuarantineStatus::CheckFailed { .. } => {}
            got => panic!("expected forced CheckFailed, got {got:?}"),
        }

        std::env::remove_var("AIKI_TEST_QUARANTINE_STATUS");
        assert_eq!(check(file.path()), QuarantineStatus::NotQuarantined);
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use super::*;
        use std::ffi::CString;

        fn set_quarantine(path: &Path, value: &str) {
            let c_path = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            let rc = unsafe {
                libc::setxattr(
                    c_path.as_ptr(),
                    XATTR_NAME.as_ptr(),
                    value.as_ptr() as *const libc::c_void,
                    value.len(),
                    0,
                    0,
                )
            };
            assert_eq!(
                rc,
                0,
                "setxattr failed: {}",
                std::io::Error::last_os_error()
            );
        }

        #[test]
        fn check_reports_pending_xattr() {
            let file = tempfile::NamedTempFile::new().unwrap();
            let value = "0083;689ab12c;Chrome;F5A19714-2C21-4E45-8189-A6B7E05C7E2E";
            set_quarantine(file.path(), value);
            assert_eq!(
                check(file.path()),
                QuarantineStatus::Pending { raw: value.to_string() }
            );
        }

        #[test]
        fn check_reports_approved_xattr() {
            let file = tempfile::NamedTempFile::new().unwrap();
            set_quarantine(
                file.path(),
                "01c1;689ab12c;Safari;F5A19714-2C21-4E45-8189-A6B7E05C7E2E",
            );
            assert_eq!(check(file.path()), QuarantineStatus::Approved);
        }

        #[test]
        fn strip_removes_xattr() {
            let file = tempfile::NamedTempFile::new().unwrap();
            let value = "0083;689ab12c;Chrome;F5A19714-2C21-4E45-8189-A6B7E05C7E2E";
            set_quarantine(file.path(), value);
            assert_eq!(
                check(file.path()),
                QuarantineStatus::Pending { raw: value.to_string() }
            );

            strip(file.path()).unwrap();
            assert_eq!(check(file.path()), QuarantineStatus::NotQuarantined);
        }

        #[test]
        fn check_reports_not_quarantined_without_xattr() {
            let file = tempfile::NamedTempFile::new().unwrap();
            assert_eq!(check(file.path()), QuarantineStatus::NotQuarantined);
        }

        #[test]
        fn check_reports_check_failed_on_missing_path() {
            match check(Path::new("/nonexistent/aiki-quarantine-test/missing")) {
                QuarantineStatus::CheckFailed { errno } => assert_eq!(errno, libc::ENOENT),
                other => panic!("expected CheckFailed, got {other:?}"),
            }
        }
    }
}
