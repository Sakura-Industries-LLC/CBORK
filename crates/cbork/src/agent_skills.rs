// Copyright (c) 2026 Sakura Industries LLC.
//
// SPDX-License-Identifier: AGPL-3.0-only

//! Agent-skill bundle management for `cbork agent skills`.
//!
//! Skill files are embedded at build time from `crates/cbork/assets/agent-skills/`
//! so the installed or published `cbork` binary does not depend on the source
//! checkout of `.agents/skills/cddl/` at runtime. This module reconciles the
//! embedded files against a chosen destination directory.
//!
//! Streaming convention: per-file warnings go to `stderr`; the final summary
//! line goes to `stdout`. The same convention is used across all five modes
//! (`install` (default), `--overwrite`, `--merge`, `--clean`, `--check`) so
//! CI output is deterministic.

#[allow(clippy::unreadable_literal, clippy::redundant_locals)]
/// Generated manifest module containing the skill bundle data.
mod generated {
    include!(concat!(env!("OUT_DIR"), "/agent_skills_manifest.rs"));
}

use std::{
    collections::HashSet,
    ffi::OsString,
    fs, io,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use console::style;
use diffy::{create_patch, merge, merge_bytes};
use generated::{SKILL_BUNDLES, SkillBundle, SkillFile};

/// Tool-owned state directory under the user-selected destination.
///
/// Excluded from the extras scan because it lives next to managed skill
/// files but is not part of the embedded manifest.
const STATE_DIR_NAME: &str = ".cbork-agent-skills";

/// One skill-management invocation.
#[derive(Debug, Clone)]
pub(crate) struct SkillOperation {
    /// Top-level destination directory supplied by the user (or the default).
    pub(crate) destination: PathBuf,
    /// Mode selected by command-line flags.
    pub(crate) mode: SkillMode,
}

/// Skill-management mode. Exactly one is active per invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillMode {
    /// Default install: missing files copied; differing files prompt.
    Install,
    /// Check-only mode: never write; non-zero on stale or missing managed files.
    Check,
    /// Replace differing files with the bundled bytes.
    Overwrite,
    /// Three-way merge using the stored ancestor.
    Merge,
    /// Install plus remove direct extras (non-recursive by design).
    Clean,
}

impl SkillMode {
    /// Stable lowercase name used in summary lines.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Check => "check",
            Self::Overwrite => "overwrite",
            Self::Merge => "merge",
            Self::Clean => "clean",
        }
    }
}

/// Per-file outcomes aggregated across the whole operation.
#[derive(Debug, Default)]
pub(crate) struct Report {
    /// Files newly installed.
    pub(crate) installed: Vec<String>,
    /// Files that already matched the bundled bytes.
    pub(crate) unchanged: Vec<String>,
    /// Files overwritten with the bundled bytes (`--overwrite` or interactive replace).
    pub(crate) replaced: Vec<String>,
    /// Files three-way-merged without conflict.
    pub(crate) merged: Vec<String>,
    /// Files skipped at the user's request (interactive `s`, or quit).
    pub(crate) skipped: Vec<String>,
    /// Files left in a differing state after this operation.
    pub(crate) differ: Vec<String>,
    /// Hard errors (filesystem I/O, merge conflicts, invalid paths).
    pub(crate) errors: Vec<String>,
    /// Soft warnings (extra files, retained directories).
    pub(crate) warnings: Vec<String>,
    /// Files removed by `--clean`.
    pub(crate) removed: Vec<String>,
    /// `true` when every expected write or check succeeded.
    pub(crate) ok: bool,
}

impl Report {
    /// Merge `other` into `self`, accumulating fields and `AND`-ing the success flag.
    fn merge(
        &mut self,
        other: Report,
    ) {
        self.installed.extend(other.installed);
        self.unchanged.extend(other.unchanged);
        self.replaced.extend(other.replaced);
        self.merged.extend(other.merged);
        self.skipped.extend(other.skipped);
        self.differ.extend(other.differ);
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
        self.removed.extend(other.removed);
        self.ok &= other.ok;
    }

    /// Print warnings and errors to stderr and a single aggregate summary line to stdout.
    pub(crate) fn print(
        &self,
        mode: SkillMode,
        destination: &Path,
    ) {
        for warning in &self.warnings {
            eprintln!("{}: {warning}", style("warning").yellow());
        }
        for err in &self.errors {
            eprintln!("{}: {err}", style("error").red());
        }
        println!(
            "{}: mode={} destination={} installed={} unchanged={} replaced={} merged={} \
             skipped={} differ={} removed={} warnings={} errors={} ok={}",
            style("cbork agent skills").bold(),
            mode.name(),
            destination.display(),
            self.installed.len(),
            self.unchanged.len(),
            self.replaced.len(),
            self.merged.len(),
            self.skipped.len(),
            self.differ.len(),
            self.removed.len(),
            self.warnings.len(),
            self.errors.len(),
            self.ok,
        );
    }
}

/// Run the operation and produce a complete report.
///
/// Returns `Err` only for unexpected runtime errors (e.g. programmer bugs);
/// all expected user-facing failures land in `Report.errors` with `ok = false`.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn execute(operation: &SkillOperation) -> Result<Report> {
    let mut report = Report {
        ok: true,
        ..Report::default()
    };

    if SKILL_BUNDLES.is_empty() {
        report
            .warnings
            .push("no skill bundles are embedded in this build".to_string());
        return Ok(report);
    }

    for bundle in SKILL_BUNDLES {
        match process_bundle(bundle, operation) {
            Ok(sub) => report.merge(sub),
            Err(err) => {
                report.ok = false;
                report.errors.push(format!("bundle {:?}: {err}", bundle.id));
            },
        }
    }

    Ok(report)
}

/// Run one bundle against the destination.
fn process_bundle(
    bundle: &SkillBundle,
    op: &SkillOperation,
) -> Result<Report> {
    let bundle_dest = bundle_destination(&op.destination, bundle.id);
    let state_root = op.destination.join(STATE_DIR_NAME).join("state");

    let mut report = Report {
        ok: true,
        ..Report::default()
    };

    let writes = !matches!(op.mode, SkillMode::Check);

    if writes && !bundle_dest.exists() {
        fs::create_dir_all(&bundle_dest)
            .with_context(|| format!("creating {}", bundle_dest.display()))?;
    }

    if writes {
        fs::create_dir_all(&state_root)
            .with_context(|| format!("creating {}", state_root.display()))?;
    }

    let stdin_is_terminal = io::stdin().is_terminal();
    let mut quit_requested = false;

    for file in bundle.files {
        let rel = file.relative_path.to_string();
        let target = bundle_dest.join(file.relative_path);
        let ancestor_path = state_root.join(bundle.id).join(file.relative_path);

        match read_target(&target)? {
            TargetState::Missing => {
                if op.mode == SkillMode::Check {
                    eprintln!("{} {}", style("missing file").red(), rel);
                    report.differ.push(rel);
                    report.ok = false;
                } else {
                    install_file(&target, file.bytes, &ancestor_path, &mut report)?;
                    report.installed.push(rel);
                }
            },
            TargetState::Directory => {
                report.errors.push(format!(
                    "{rel}: destination is a directory, refusing to replace"
                ));
                report.ok = false;
            },
            TargetState::NonRegular => {
                report
                    .errors
                    .push(format!("{rel}: destination is not a regular file"));
                report.ok = false;
            },
            TargetState::Regular(existing) if existing == *file.bytes => {
                report.unchanged.push(rel);
            },
            TargetState::Regular(existing) => {
                handle_differing(
                    &mut report,
                    file,
                    &rel,
                    &target,
                    &ancestor_path,
                    &existing,
                    op.mode,
                    stdin_is_terminal,
                )?;
                if report.skipped.last().is_some_and(|last| last == &rel) && !report.ok {
                    quit_requested = true;
                }
            },
        }

        if quit_requested {
            break;
        }
    }

    if !quit_requested {
        run_extras_pass(&mut report, &bundle_dest, bundle, op.mode)?;
    }

    Ok(report)
}

/// Reconcile a single differing file against the active mode.
#[allow(clippy::too_many_arguments)]
fn handle_differing(
    report: &mut Report,
    file: &SkillFile,
    rel: &str,
    target: &Path,
    ancestor_path: &Path,
    existing: &[u8],
    mode: SkillMode,
    stdin_is_terminal: bool,
) -> Result<()> {
    match mode {
        SkillMode::Check => {
            emit_diff(rel, existing, file.bytes);
            report.differ.push(rel.to_string());
            report.ok = false;
        },
        SkillMode::Overwrite => {
            install_file(target, file.bytes, ancestor_path, report)?;
            report.replaced.push(rel.to_string());
        },
        SkillMode::Merge => {
            let ancestor = read_ancestor(ancestor_path)?;
            match try_merge(file.bytes, existing, ancestor.as_deref())? {
                MergeOutcome::Conflict => {
                    report.errors.push(format!(
                        "{rel}: merge conflict (local and incoming edits overlap)"
                    ));
                    report.differ.push(rel.to_string());
                    report.ok = false;
                },
                MergeOutcome::Identical => {
                    report.unchanged.push(rel.to_string());
                },
                MergeOutcome::Clean(merged) => {
                    install_file(target, &merged, ancestor_path, report)?;
                    report.merged.push(rel.to_string());
                },
            }
        },
        SkillMode::Install | SkillMode::Clean => {
            if !stdin_is_terminal {
                emit_diff(rel, existing, file.bytes);
                eprintln!(
                    "{} {}: destination file differs; rerun with --overwrite or --merge",
                    style("error").red(),
                    rel,
                );
                report.differ.push(rel.to_string());
                report.ok = false;
                return Ok(());
            }

            emit_diff(rel, existing, file.bytes);

            let ancestor = read_ancestor(ancestor_path)?;
            let has_ancestor = ancestor.is_some();

            match prompt_choice(has_ancestor)? {
                Choice::Merge => {
                    match try_merge(file.bytes, existing, ancestor.as_deref())? {
                        MergeOutcome::Conflict => {
                            report.errors.push(format!(
                                "{rel}: merge conflict (local and incoming edits overlap)"
                            ));
                            report.differ.push(rel.to_string());
                            report.ok = false;
                        },
                        MergeOutcome::Identical => {
                            report.unchanged.push(rel.to_string());
                        },
                        MergeOutcome::Clean(merged) => {
                            install_file(target, &merged, ancestor_path, report)?;
                            report.merged.push(rel.to_string());
                        },
                    }
                },
                Choice::Replace => {
                    install_file(target, file.bytes, ancestor_path, report)?;
                    report.replaced.push(rel.to_string());
                },
                Choice::Skip => {
                    report.skipped.push(rel.to_string());
                },
                Choice::Quit => {
                    report.skipped.push(rel.to_string());
                    report.ok = false;
                },
            }
        },
    }
    Ok(())
}

/// Walk the destination's direct children and either warn about extras or remove them.
///
/// The `--clean` removal is deliberately non-recursive:
/// only direct children under the selected destination directory are removed.
/// Nested content inside an unexpected subdirectory is never recursively deleted;
/// the directory is retained and a warning is emitted instead.
#[allow(clippy::unnecessary_wraps)]
fn run_extras_pass(
    report: &mut Report,
    bundle_dest: &Path,
    bundle: &SkillBundle,
    mode: SkillMode,
) -> Result<()> {
    let extras = match scan_extras(bundle_dest, bundle) {
        Ok(extras) => extras,
        Err(err) => {
            report.errors.push(format!(
                "extras scan failed for {}: {err}",
                bundle_dest.display()
            ));
            report.ok = false;
            return Ok(());
        },
    };

    if extras.is_empty() {
        return Ok(());
    }

    match mode {
        SkillMode::Clean => {
            for extra in extras {
                let rel_display = extra
                    .strip_prefix(bundle_dest)
                    .unwrap_or(&extra)
                    .to_string_lossy()
                    .into_owned();
                match clean_extra(&extra) {
                    CleanAction::Removed => {
                        report.removed.push(rel_display);
                    },
                    CleanAction::RetainedNonEmptyDir => {
                        report.warnings.push(format!(
                            "{rel_display}: directory retained (contains files cbork does not manage)"
                        ));
                    },
                    CleanAction::Skipped => {},
                }
            }
        },
        _ => {
            for extra in extras {
                let rel_display = extra
                    .strip_prefix(bundle_dest)
                    .unwrap_or(&extra)
                    .to_string_lossy()
                    .into_owned();
                let kind = match fs::symlink_metadata(&extra) {
                    Ok(meta) if meta.file_type().is_symlink() => "symlink",
                    Ok(meta) if meta.is_dir() => "directory",
                    Ok(_) => "file",
                    Err(_) => "entry",
                };
                report
                    .warnings
                    .push(format!("extra {kind} present: {rel_display}"));
            }
        },
    }

    Ok(())
}

/// Classify the kind of an extra entry under `--clean`.
enum CleanAction {
    /// The entry was removed successfully.
    Removed,
    /// A non-empty directory that was retained due to content.
    RetainedNonEmptyDir,
    /// The entry was skipped (symlink, unreadable, or non-removable).
    Skipped,
}

/// Remove an unexpected entry, if it is safe to do so.
///
/// Skips symlinks (they are never managed) and entries we cannot classify.
fn clean_extra(path: &Path) -> CleanAction {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return CleanAction::Skipped;
    };
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        return CleanAction::Skipped;
    }
    if meta.is_file() {
        return if fs::remove_file(path).is_ok() {
            CleanAction::Removed
        } else {
            CleanAction::Skipped
        };
    }
    if meta.is_dir() {
        let empty = fs::read_dir(path).is_ok_and(|mut it| it.next().is_none());
        return if empty {
            if fs::remove_dir(path).is_ok() {
                CleanAction::Removed
            } else {
                CleanAction::Skipped
            }
        } else {
            CleanAction::RetainedNonEmptyDir
        };
    }
    CleanAction::Skipped
}

/// List direct-child entries under `bundle_dest` that are not part of the
/// embedded manifest and not the tool-owned state directory.
fn scan_extras(
    bundle_dest: &Path,
    bundle: &SkillBundle,
) -> Result<Vec<PathBuf>> {
    let mut managed = HashSet::new();
    for file in bundle.files {
        managed.insert(file.relative_path.to_string());
    }

    let mut extras = Vec::new();

    match fs::read_dir(bundle_dest) {
        Ok(entries) => {
            for entry in entries {
                let entry =
                    entry.map_err(|err| anyhow!("iterating {}: {err}", bundle_dest.display()))?;
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if name == STATE_DIR_NAME {
                    continue;
                }
                if managed.contains(&name) {
                    continue;
                }
                extras.push(entry.path());
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => {},
        Err(err) => {
            return Err(anyhow!("reading {}: {err}", bundle_dest.display()));
        },
    }

    Ok(extras)
}

/// Compute the per-bundle destination directory.
fn bundle_destination(
    root: &Path,
    bundle_id: &str,
) -> PathBuf {
    root.join(bundle_id)
}

/// Read an existing target file and decide what kind of entry it is.
fn read_target(path: &Path) -> Result<TargetState> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(TargetState::Missing),
        Err(err) => return Err(anyhow!("stat {}: {err}", path.display())),
    };

    let file_type = meta.file_type();
    if file_type.is_symlink() {
        return Ok(TargetState::NonRegular);
    }
    if meta.is_dir() {
        return Ok(TargetState::Directory);
    }
    if !meta.is_file() {
        return Ok(TargetState::NonRegular);
    }

    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(TargetState::Regular(bytes))
}

/// Classification of an existing entry at a managed file path.
enum TargetState {
    /// The path does not exist on disk.
    Missing,
    /// The path is a directory, which cannot be replaced by a managed file.
    Directory,
    /// The path is a symlink or other non-regular entry.
    NonRegular,
    /// The path is a regular file with the given byte content.
    Regular(Vec<u8>),
}

/// Atomically install a managed file, then update the local state ancestor.
///
/// Writes go through a temp file in the destination directory before the final
/// rename so an interrupted operation cannot leave a partially-written file
/// at the destination path. State writes are best-effort: failure there is
/// reported as a warning but does not fail the install.
fn install_file(
    target: &Path,
    bytes: &[u8],
    ancestor_path: &Path,
    report: &mut Report,
) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    let tmp = temp_sibling_path(target);
    fs::write(&tmp, bytes).with_context(|| format!("writing temp file {}", tmp.display()))?;
    if let Err(err) = fs::rename(&tmp, target) {
        drop(fs::remove_file(&tmp));
        return Err(anyhow!(
            "renaming {} to {}: {err}",
            tmp.display(),
            target.display()
        ));
    }

    if let Some(parent) = ancestor_path.parent() {
        drop(fs::create_dir_all(parent));
    }
    let state_tmp = temp_sibling_path(ancestor_path);
    if let Err(err) = fs::write(&state_tmp, bytes) {
        report.warnings.push(format!(
            "state write to {} failed: {err}",
            ancestor_path.display()
        ));
        return Ok(());
    }
    if let Err(err) = fs::rename(&state_tmp, ancestor_path) {
        drop(fs::remove_file(&state_tmp));
        report.warnings.push(format!(
            "state rename to {} failed: {err}",
            ancestor_path.display()
        ));
    }
    Ok(())
}

/// Build a sibling temp file path unique to this process and the target.
fn temp_sibling_path(target: &Path) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let original = target
        .file_name()
        .map_or_else(|| OsString::from("cbork"), OsString::from);
    let mut name = original;
    name.push(format!(".cbork-tmp-{}.partial", std::process::id()));
    parent.join(name)
}

/// Read the stored ancestor bytes for a file, if present.
fn read_ancestor(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow!("reading ancestor {}: {err}", path.display())),
    }
}

/// Three-way merge result.
enum MergeOutcome {
    /// The merge completed and produced bytes that may differ from the local side.
    Clean(Vec<u8>),
    /// `local` and `incoming` are identical; nothing needs to be written.
    Identical,
    /// `diffy` returned `Err`. Treat as an unresolved conflict.
    Conflict,
}

/// Run a three-way merge using `diffy`.
///
/// `ancestor` is the previously installed vendored bytes (or `None`).
/// `local` is the destination file. `incoming` is the new vendored bytes.
fn try_merge(
    incoming: &[u8],
    local: &[u8],
    ancestor: Option<&[u8]>,
) -> Result<MergeOutcome> {
    let Some(ancestor) = ancestor else {
        return Ok(MergeOutcome::Conflict);
    };

    if local == incoming {
        return Ok(MergeOutcome::Identical);
    }

    let utf8_in = is_utf8(incoming);
    let utf8_local = is_utf8(local);
    let utf8_ancestor = is_utf8(ancestor);

    if utf8_in && utf8_local && utf8_ancestor {
        let incoming_s =
            std::str::from_utf8(incoming).map_err(|_| anyhow!("bad utf-8 incoming"))?;
        let local_s = std::str::from_utf8(local).map_err(|_| anyhow!("bad utf-8 local"))?;
        let ancestor_s =
            std::str::from_utf8(ancestor).map_err(|_| anyhow!("bad utf-8 ancestor"))?;
        match merge(ancestor_s, local_s, incoming_s) {
            Ok(merged) => Ok(MergeOutcome::Clean(merged.into_bytes())),
            Err(_) => Ok(MergeOutcome::Conflict),
        }
    } else {
        match merge_bytes(ancestor, local, incoming) {
            Ok(merged) => Ok(MergeOutcome::Clean(merged.clone())),
            Err(_) => Ok(MergeOutcome::Conflict),
        }
    }
}

/// Emit a unified diff between the local and vendored bytes to stderr.
///
/// Only produces a textual diff when both buffers are valid UTF-8.
/// For binary content, falls back to a byte-count summary.
fn emit_diff(
    rel: &str,
    existing: &[u8],
    vendored: &[u8],
) {
    if let (Ok(a), Ok(b)) = (std::str::from_utf8(existing), std::str::from_utf8(vendored)) {
        let patch = create_patch(a, b);
        eprintln!("{} {}", style("diff").bold(), rel);
        eprint!("{patch}");
    } else {
        eprintln!(
            "{} {}: binary content differs (existing {} bytes, vendored {} bytes)",
            style("note").yellow(),
            rel,
            existing.len(),
            vendored.len()
        );
    }
}

/// `true` when `bytes` is valid UTF-8.
fn is_utf8(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok()
}

/// One menu option for the interactive prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    /// Run a three-way merge against the stored ancestor.
    Merge,
    /// Replace the destination file with the vendored bytes.
    Replace,
    /// Leave the file unchanged and continue.
    Skip,
    /// Stop processing and return failure.
    Quit,
}

/// Read an interactive choice from stdin.
///
/// Returns `Skip` on EOF or empty input. Reprompts on invalid input.
/// Hides the `merge` option when `has_ancestor` is `false`.
fn prompt_choice(has_ancestor: bool) -> io::Result<Choice> {
    loop {
        if has_ancestor {
            eprint!("[m]erge  [r]eplace  [s]kip  [q]uit > ");
        } else {
            eprint!("[r]eplace  [s]kip  [q]uit > ");
        }
        io::stderr().flush()?;

        let mut line = String::new();
        let read = io::stdin().read_line(&mut line)?;
        if read == 0 {
            return Ok(Choice::Skip);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(Choice::Skip);
        }

        match trimmed {
            "m" | "M" if has_ancestor => return Ok(Choice::Merge),
            "r" | "R" => return Ok(Choice::Replace),
            "s" | "S" => return Ok(Choice::Skip),
            "q" | "Q" => return Ok(Choice::Quit),
            "m" | "M" => {
                eprintln!(
                    "{}: no stored ancestor for this file; merge unavailable",
                    style("note").yellow()
                );
                return prompt_choice(false);
            },
            _ => {
                eprintln!(
                    "invalid choice: {trimmed:?}; expected one of {}",
                    if has_ancestor {
                        "m, r, s, q"
                    } else {
                        "r, s, q"
                    }
                );
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::missing_docs_in_private_items)]
mod tests {
    use std::path::PathBuf;

    /// Verify that every managed file in `assets/agent-skills/cddl/` matches
    /// the canonical source under `.agents/skills/cddl/`.
    ///
    /// This test reads the canonical `.agents/skills/` directory at test time
    /// so any divergence from the bundled copy is caught early.
    #[test]
    fn canonical_skills_match_embedded_manifest() {
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .map_or_else(|| PathBuf::from("."), PathBuf::from);

        let canonical = workspace_root.join(".agents/skills/cddl");

        if !canonical.is_dir() {
            return;
        }

        let bundle = super::SKILL_BUNDLES
            .iter()
            .find(|b| b.id == "cddl")
            .expect("expected cddl bundle in embedded manifest");

        let mut count = 0;
        for file in bundle.files {
            let canonical_path = canonical.join(file.relative_path);
            let on_disk = std::fs::read(&canonical_path).unwrap_or_else(|err| {
                panic!(
                    "failed to read canonical skill file {}: {err}",
                    canonical_path.display()
                )
            });

            assert_eq!(
                on_disk, file.bytes,
                "canonical source {:?} differs from embedded manifest; \
                 sync with: cp -R .agents/skills/cddl/* crates/cbork/assets/agent-skills/cddl/",
                file.relative_path,
            );
            count += 1;
        }

        assert!(
            count > 0,
            "expected at least one skill file in the cddl bundle"
        );
    }
}
