// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: MPL-2.0

//! Compile-time catalog of CDDL standard modules from `cddl/rfc-std/`.
//!
//! Generated at build time; provides O(1) content lookup by built-in name
//! and an ordered list of all known names.

use std::fmt::Write as _;

include!(concat!(env!("OUT_DIR"), "/rfc_std_catalog.rs"));

/// Look up the CDDL content for a built-in standard module by name.
///
/// Returns `None` if the name is not a known built-in module.
///
/// # Examples
///
/// ```
/// # use cbork_catalog;
/// let content = cbork_catalog::lookup("rfc9052");
/// assert!(content.is_some());
/// assert!(cbork_catalog::lookup("nonexistent").is_none());
/// ```
#[must_use]
pub fn lookup(name: &str) -> Option<&'static str> {
    CATALOG.get(name).copied()
}

/// Return an iterator over all known built-in module names in sorted order.
///
/// # Examples
///
/// ```
/// # use cbork_catalog;
/// let names: Vec<_> = cbork_catalog::known_names().collect();
/// assert!(names.contains(&"rfc9052"));
/// assert!(names.contains(&"rfc8727"));
/// ```
pub fn known_names() -> impl Iterator<Item = &'static str> {
    KNOWN_NAMES.iter().copied()
}

/// Return a human-readable summary of the catalog for diagnostic output.
///
/// # Examples
///
/// ```
/// # use cbork_catalog;
/// let summary = cbork_catalog::summary();
/// assert!(summary.contains("rfc9052"));
/// ```
#[must_use]
pub fn summary() -> String {
    let mut s = String::from("built-in standard modules:\n");
    for name in KNOWN_NAMES {
        let _ = writeln!(s, "  {name}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_name_lookup() {
        let content = lookup("rfc9052");
        assert!(content.is_some());
        assert!(content.unwrap().contains("COSE_Key"));
    }

    #[test]
    fn unknown_name_fails() {
        assert!(lookup("nonexistent-module").is_none());
    }

    #[test]
    fn name_listing() {
        let names: Vec<_> = known_names().collect();
        assert!(!names.is_empty());
        assert!(names.contains(&"rfc8727"));
        assert!(names.contains(&"rfc9052"));
    }

    #[test]
    fn stable_mapping() {
        // The catalog must be stable: same name always returns same content
        let a = lookup("rfc8610");
        let b = lookup("rfc8610");
        assert_eq!(a, b);
    }
}
