//! Saved source replacements and the diffs included in agent feedback.

use super::*;
use std::ops::Range;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BlockEdit {
    /// Complete source lines, including a list item's marker and indentation.
    pub before: String,
    pub after: String,
    /// Location in the document immediately before this edit.
    pub start_line: usize,
    pub end_line: usize,
}

impl BlockEdit {
    fn from_replacement(source: &str, range: Range<usize>, text: &str) -> Self {
        let start = source[..range.start].rfind('\n').map_or(0, |at| at + 1);
        let end = if range.end > 0 && source.as_bytes()[range.end - 1] == b'\n' {
            range.end
        } else {
            source[range.end..]
                .find('\n')
                .map_or(source.len(), |at| range.end + at + 1)
        };
        let before = source[start..end].to_owned();
        let after = format!(
            "{}{}{}",
            &source[start..range.start],
            text,
            &source[range.end..end]
        );
        let start_line = source[..start]
            .bytes()
            .filter(|&byte| byte == b'\n')
            .count()
            + 1;
        let end_line = start_line + before.split_inclusive('\n').count().max(1) - 1;
        Self {
            before,
            after,
            start_line,
            end_line,
        }
    }

    /// A standard unified diff, with three unchanged lines around each hunk.
    pub fn unified_diff(&self) -> String {
        let old: Vec<_> = self.before.split_inclusive('\n').collect();
        let new: Vec<_> = self.after.split_inclusive('\n').collect();
        let ops = line_diff(&old, &new);
        let mut hunks: Vec<Range<usize>> = Vec::new();
        for (at, (kind, _)) in ops.iter().enumerate() {
            if *kind == ' ' {
                continue;
            }
            let start = at.saturating_sub(3);
            let end = (at + 4).min(ops.len());
            if let Some(last) = hunks.last_mut()
                && start <= last.end
            {
                last.end = end;
            } else {
                hunks.push(start..end);
            }
        }
        if hunks.is_empty() {
            return String::new();
        }
        let mut diff = String::from("--- original\n+++ edited\n");
        for hunk in hunks {
            let old_before = ops[..hunk.start]
                .iter()
                .filter(|(kind, _)| *kind != '+')
                .count();
            let new_before = ops[..hunk.start]
                .iter()
                .filter(|(kind, _)| *kind != '-')
                .count();
            let old_count = ops[hunk.clone()]
                .iter()
                .filter(|(kind, _)| *kind != '+')
                .count();
            let new_count = ops[hunk.clone()]
                .iter()
                .filter(|(kind, _)| *kind != '-')
                .count();
            let old_start =
                self.start_line.saturating_sub(1) + old_before + usize::from(old_count > 0);
            let new_start =
                self.start_line.saturating_sub(1) + new_before + usize::from(new_count > 0);
            diff.push_str(&format!(
                "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
            ));
            for (kind, line) in &ops[hunk] {
                diff.push(*kind);
                diff.push_str(line);
                if !line.ends_with('\n') {
                    diff.push_str("\n\\ No newline at end of file\n");
                }
            }
        }
        diff
    }
}

fn line_diff<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(char, &'a str)> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let a = &old[prefix..old.len() - suffix];
    let b = &new[prefix..new.len() - suffix];
    let mut ops: Vec<_> = old[..prefix].iter().map(|&line| (' ', line)).collect();
    // Bound memory for a wholesale rewrite of a very large block. That case
    // still produces a complete patch, with the middle replaced as one hunk.
    if (a.len() + 1).saturating_mul(b.len() + 1) <= 1_000_000 {
        let width = b.len() + 1;
        let mut lcs = vec![0u32; (a.len() + 1) * width];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                lcs[i * width + j] = if a[i] == b[j] {
                    1 + lcs[(i + 1) * width + j + 1]
                } else {
                    lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                ops.push((' ', a[i]));
                i += 1;
                j += 1;
            } else if i < a.len()
                && (j == b.len() || lcs[(i + 1) * width + j] >= lcs[i * width + j + 1])
            {
                ops.push(('-', a[i]));
                i += 1;
            } else {
                ops.push(('+', b[j]));
                j += 1;
            }
        }
    } else {
        ops.extend(a.iter().map(|&line| ('-', line)));
        ops.extend(b.iter().map(|&line| ('+', line)));
    }
    ops.extend(old[old.len() - suffix..].iter().map(|&line| (' ', line)));
    ops
}

/// Journal a source replacement before writing it, so an edit-only page is
/// feedback even when the user closes and reopens the viewer before submitting.
pub fn replace_block_with_feedback(
    request: &crate::transport::Handoff,
    source: &str,
    range: Range<usize>,
    text: &str,
) -> Result<(String, usize), Box<dyn std::error::Error>> {
    let path = request.source.as_ref().unwrap_or(&request.path);
    let revision = FileRevision::open_checked(path, &source_hash(source.as_bytes()))?;
    let original = source
        .get(range.clone())
        .ok_or("block source range is invalid")?;
    let journal_path = sidecar_path(&request.path);
    let saved = AgentReplies::load(&journal_path)?;
    let mut journal = saved.clone();
    let changed = original != text;
    if changed {
        let edit = BlockEdit::from_replacement(source, range.clone(), text);
        if let Some(last) = journal.edits.last_mut()
            && last.start_line == edit.start_line
            && last.after == edit.before
        {
            last.after = edit.after;
            if last.before == last.after {
                journal.edits.pop();
            }
        } else {
            journal.edits.push(edit);
        }
        journal.save(&journal_path)?;
    }
    let updated = match revision.replace_range(range, text) {
        Ok(updated) => updated,
        Err(error) => {
            if changed && let Err(restore) = saved.save(&journal_path) {
                return Err(format!("{error}; could not restore edit journal: {restore}").into());
            }
            return Err(error);
        }
    };
    Ok((
        updated,
        journal
            .edits
            .iter()
            .filter(|edit| edit.before != edit.after)
            .count(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Scheme;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "limpid-edits-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn handoff(path: &Path) -> Handoff {
        Handoff {
            path: path.into(),
            source: Some(path.into()),
            source_hash: None,
            source_backed: false,
            pane: None,
            theme: Scheme::Dark,
        }
    }

    #[test]
    fn edits_are_saved_reopened_coalesced_and_removed_when_reverted() {
        let dir = scratch();
        let path = dir.join("page.md");
        let request = handoff(&path);
        fs::write(&path, "before\n").unwrap();
        let (next, count) =
            replace_block_with_feedback(&request, "before\n", 0..7, "middle\n").unwrap();
        assert_eq!(count, 1);
        assert!(has_feedback(&request).unwrap());
        let saved = saved_replies(&request).unwrap();
        assert_eq!(saved.edits[0].before, "before\n");
        assert_eq!(saved.edits[0].after, "middle\n");
        let (next, count) =
            replace_block_with_feedback(&request, &next, 0..next.len(), "after\n").unwrap();
        assert_eq!(count, 1);
        let saved = saved_replies(&request).unwrap();
        assert_eq!(saved.edits[0].before, "before\n");
        assert_eq!(saved.edits[0].after, "after\n");
        assert!(
            compose_review(&request, "page")
                .unwrap()
                .contains("-before\n+after\n")
        );
        let (_, count) =
            replace_block_with_feedback(&request, &next, 0..next.len(), "before\n").unwrap();
        assert_eq!(count, 0);
        assert!(!has_feedback(&request).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn noops_and_stale_files_do_not_create_edit_feedback() {
        let dir = scratch();
        let path = dir.join("page.md");
        let request = handoff(&path);
        fs::write(&path, "before\n").unwrap();
        assert_eq!(
            replace_block_with_feedback(&request, "before\n", 0..7, "before\n")
                .unwrap()
                .1,
            0
        );
        assert!(!sidecar_path(&path).exists());
        fs::write(&path, "elsewhere\n").unwrap();
        assert!(replace_block_with_feedback(&request, "before\n", 0..7, "after\n").is_err());
        assert!(!sidecar_path(&path).exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "elsewhere\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unwritable_journal_prevents_the_source_edit() {
        let dir = scratch();
        let path = dir.join("page.md");
        let request = handoff(&path);
        fs::write(&path, "before\n").unwrap();
        fs::create_dir(sidecar_path(&path)).unwrap();
        assert!(replace_block_with_feedback(&request, "before\n", 0..7, "after\n").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn line_diffs_keep_list_markers_and_apply_to_the_whole_document() {
        let source = "# Plan\n\n- original café\n- untouched\n";
        let start = source.find("original").unwrap();
        let edit = BlockEdit::from_replacement(
            source,
            start..start + "original café".len(),
            "revised café",
        );
        assert_eq!(edit.before, "- original café\n");
        assert_eq!(edit.after, "- revised café\n");
        assert_eq!((edit.start_line, edit.end_line), (3, 3));
        let diff = edit.unified_diff();
        assert!(
            diff.contains("@@ -3,1 +3,1 @@\n-- original café\n+- revised café\n"),
            "{diff}"
        );
        apply_patch_check(source, &source.replace("original", "revised"), &diff);
    }

    fn apply_patch_check(before: &str, after: &str, diff: &str) {
        if Command::new("patch").arg("--version").output().is_err() {
            return;
        }
        let dir = scratch();
        let path = dir.join("page.md");
        let patch = dir.join("change.diff");
        fs::write(&path, before).unwrap();
        fs::write(&patch, diff).unwrap();
        let result = Command::new("patch")
            .arg("--batch")
            .arg(&path)
            .arg(&patch)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{} {}\n{diff}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), after);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn patches_cover_separated_changes_insertions_deletions_and_missing_final_newlines() {
        let cases = [
            (
                "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\no\np\n",
                "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\nO\np\n",
            ),
            ("before\n", "before\nadded\n"),
            ("before\nremoved\n", "before\n"),
            ("before", "after"),
            ("before\n", "after"),
            ("before", "after\n"),
            ("before\n", ""),
        ];
        for (before, after) in cases {
            let edit = BlockEdit {
                before: before.into(),
                after: after.into(),
                start_line: 1,
                end_line: before.lines().count().max(1),
            };
            apply_patch_check(before, after, &edit.unified_diff());
        }
    }

    #[test]
    fn large_rewrites_use_a_complete_patch_with_bounded_diff_memory() {
        let before = "old\n".repeat(1100);
        let after = "new\n".repeat(1100);
        let edit = BlockEdit {
            before: before.clone(),
            after: after.clone(),
            start_line: 1,
            end_line: 1100,
        };
        apply_patch_check(&before, &after, &edit.unified_diff());
    }
}
