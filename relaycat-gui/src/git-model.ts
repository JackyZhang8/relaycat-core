export interface GitActionPolicy {
  stage: { requiresConfirm: boolean };
  unstage: { requiresConfirm: boolean };
  discard: { requiresConfirm: boolean };
}

export function changeActionPolicy(_change: { conflict: boolean }): GitActionPolicy {
  return {
    stage: { requiresConfirm: false },
    unstage: { requiresConfirm: false },
    discard: { requiresConfirm: true },
  };
}

export function canCommit(message: string, stagedCount: number, amend: boolean): boolean {
  return message.trim().length > 0 && (stagedCount > 0 || amend);
}

export function gitOperationControls(running: boolean) {
  return {
    fetchDisabled: running,
    pullDisabled: running,
    pushDisabled: running,
    branchDisabled: running,
  };
}

export function historyFilterArgs(query: string, author: string, reference: string) {
  const value = (input: string) => input.trim() || null;
  return {
    query: value(query),
    author: value(author),
    reference: value(reference),
  };
}

export function reconcileGitChangeKeys(existing: string[], next: string[]) {
  const existingSet = new Set(existing);
  const nextSet = new Set(next);
  return {
    retained: existing.filter((key) => nextSet.has(key)),
    added: next.filter((key) => !existingSet.has(key)),
    removed: existing.filter((key) => !nextSet.has(key)),
  };
}

export function gitMutationUsesInlineFeedback(command: string): boolean {
  return new Set(["git_stage_paths", "git_unstage_paths", "git_apply_patch"]).has(command);
}

export function gitSyncButtonLabels(ahead: number, behind: number) {
  const count = (label: string, value: number) =>
    Number.isFinite(value) && value > 0 ? `${label}(${Math.floor(value)})` : label;
  return {
    pull: count("Pull", behind),
    push: count("Push", ahead),
  };
}

export interface GitDiffHunk {
  header: string;
  patch: string;
  lines: string[];
}

export interface GitDiffFile {
  path: string;
  header: string;
  hunks: GitDiffHunk[];
}

export function parseUnifiedDiff(content: string): GitDiffFile[] {
  const lines = content.split("\n");
  const files: GitDiffFile[] = [];
  let current: GitDiffFile | null = null;
  let headerLines: string[] = [];
  let hunkLines: string[] = [];

  const finishHunk = () => {
    if (!current || hunkLines.length === 0) return;
    const patchLines = [...headerLines, ...hunkLines];
    current.hunks.push({
      header: hunkLines[0],
      patch: `${patchLines.join("\n")}\n`,
      lines: hunkLines.slice(1),
    });
    hunkLines = [];
  };

  for (const line of lines) {
    if (line.startsWith("diff --git ")) {
      finishHunk();
      const match = /^diff --git a\/(.+) b\/(.+)$/.exec(line);
      current = { path: match?.[2] ?? "", header: "", hunks: [] };
      files.push(current);
      headerLines = [line];
    } else if (!current) {
      continue;
    } else if (line.startsWith("@@ ")) {
      finishHunk();
      current.header = headerLines.join("\n");
      hunkLines = [line];
    } else if (hunkLines.length > 0) {
      hunkLines.push(line);
    } else {
      headerLines.push(line);
    }
  }
  finishHunk();
  return files;
}

export function buildSelectedLinesPatch(
  fileHeader: string,
  hunkHeader: string,
  lines: string[],
  selected: ReadonlySet<number>,
): string {
  const match = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/.exec(hunkHeader);
  if (!match || selected.size === 0) return "";
  let oldCount = 0;
  let newCount = 0;
  const output: string[] = [];
  let previousIncluded = false;
  lines.forEach((line, index) => {
    const marker = line[0];
    if (marker === " ") {
      oldCount += 1;
      newCount += 1;
      output.push(line);
      previousIncluded = true;
    } else if (marker === "-") {
      oldCount += 1;
      newCount += selected.has(index) ? 0 : 1;
      output.push(selected.has(index) ? line : ` ${line.slice(1)}`);
      previousIncluded = true;
    } else if (marker === "+") {
      if (selected.has(index)) {
        newCount += 1;
        output.push(line);
        previousIncluded = true;
      } else {
        previousIncluded = false;
      }
    } else if (line.startsWith("\\ No newline") && previousIncluded) {
      output.push(line);
    }
  });
  const header = `@@ -${match[1]},${oldCount} +${match[2]},${newCount} @@${match[3]}`;
  return `${fileHeader}\n${header}\n${output.join("\n")}\n`;
}
