You are testing aiki's `init` command, specifically how it writes templates to `.aiki/tasks/`. Run through ALL phases carefully and report results.

## Setup
- Run `aiki task start "Test aiki init: template writing and sync" --source prompt`
- Note the task ID as PARENT

---

## Phase 1: Fresh Init — Template Files Written

Test that `aiki init` creates the expected template files in a brand-new repo.

### 1.0 Create a fresh repo

Create a throwaway directory and initialize it as a git+jj repo:

    mkdir /tmp/test-init-templates && cd /tmp/test-init-templates
    git init
    jj git init --colocate
    echo "test repo" > README.md
    jj commit -m "Initial commit"

### 1.1 Run `aiki init`

    aiki init

This should complete without errors.

### 1.2 Verify the template directory structure

Check that `.aiki/tasks/` was created with the expected files:

    ls -R .aiki/tasks/

**Expected file tree (aiki/default plugin templates):**

```
.aiki/tasks/
├── decompose.md
├── fix.md
├── loop.md
├── plan.md
├── resolve.md
├── explore/
│   ├── code.md
│   ├── plan.md
│   ├── session.md
│   └── task.md
└── review/
    ├── code.md
    ├── plan.md
    └── task.md
```

**Verify ALL 12 files exist:**
- 5 root templates: `decompose.md`, `fix.md`, `loop.md`, `plan.md`, `resolve.md`
- 4 explore templates: `explore/code.md`, `explore/plan.md`, `explore/session.md`, `explore/task.md`
- 3 review templates: `review/code.md`, `review/plan.md`, `review/task.md`

Count them:

    find .aiki/tasks -name '*.md' -not -path '*/tu/*' | wc -l
    # Should be 12

### 1.3 Verify template content has valid YAML frontmatter

Each template should be a Markdown file with YAML frontmatter (delimited by `---`). Check a sampling:

    head -6 .aiki/tasks/plan.md
    # Should show: ---\nversion: 1.0.0\ntype: plan\n...

    head -6 .aiki/tasks/review/code.md
    # Should show: ---\nversion: 3.0.0\ntype: review\n...

    head -6 .aiki/tasks/decompose.md
    # Should show frontmatter with version and type fields

**For each template checked, verify:**
- First line is `---`
- Contains a `version:` field (semantic version string)
- Contains a `type:` field (matches the template category)
- Frontmatter is closed with a second `---`
- Content after frontmatter starts with a `#` heading

### 1.4 Record phase 1 results

Note: PASS if all 12 templates exist with valid frontmatter. FAIL if any template is missing, empty, or has malformed frontmatter.

---

## Phase 2: Manifest Written Correctly

Test that `.aiki/.manifest.json` was created and accurately tracks all installed templates.

### 2.0 Verify the manifest exists

    cat .aiki/.manifest.json

### 2.1 Validate manifest structure

The manifest should be valid JSON with this structure:

```json
{
  "schema": 1,
  "templates": {
    "aiki/default": {
      "source_version": "<cli-version>",
      "install_root": ".",
      "files": {
        "<relative-path>": {
          "checksum": "sha256:<hex>",
          "version": "<semver or null>",
          "installed_at": "<ISO 8601 timestamp>"
        }
      }
    }
  }
}
```

**Verify:**
- `schema` is `1`
- `templates` has exactly one key: `"aiki/default"`
- `source_version` is a valid semver string (e.g., `"0.1.0"`)
- `install_root` is `"."`
- `files` has exactly **12 entries** (one per template)
- Each file entry has `checksum`, `version`, and `installed_at` fields
- All checksums start with `"sha256:"`
- All `installed_at` values are valid ISO 8601 timestamps

### 2.2 Verify checksums match actual file content

Pick 3 templates and verify their manifest checksums match the actual file content:

    shasum -a 256 .aiki/tasks/plan.md
    # Compare the hash with the "checksum" value in .manifest.json for "plan.md"

    shasum -a 256 .aiki/tasks/review/code.md
    # Compare with manifest entry for "review/code.md"

    shasum -a 256 .aiki/tasks/loop.md
    # Compare with manifest entry for "loop.md"

The SHA256 hashes should match (ignoring the `sha256:` prefix in the manifest).

### 2.3 Verify manifest is gitignored

    grep '.manifest.json' .gitignore
    # Should find a line matching .aiki/.manifest.json or similar

### 2.4 Record phase 2 results

Note: PASS if manifest exists, is valid JSON, has 12 file entries with correct checksums, and is gitignored. FAIL otherwise with details.

---

## Phase 3: Re-init — Idempotency (No Overwrite)

Test that running `aiki init` again does NOT overwrite existing templates and the manifest remains consistent.

### 3.0 Record pre-state

Save the current state for comparison:

    cp .aiki/.manifest.json /tmp/manifest-before.json
    shasum -a 256 .aiki/tasks/plan.md > /tmp/hashes-before.txt
    shasum -a 256 .aiki/tasks/review/code.md >> /tmp/hashes-before.txt
    shasum -a 256 .aiki/tasks/fix.md >> /tmp/hashes-before.txt

### 3.1 Run `aiki init` again

    aiki init

Should complete without errors.

### 3.2 Verify templates are unchanged

    shasum -a 256 .aiki/tasks/plan.md > /tmp/hashes-after.txt
    shasum -a 256 .aiki/tasks/review/code.md >> /tmp/hashes-after.txt
    shasum -a 256 .aiki/tasks/fix.md >> /tmp/hashes-after.txt

    diff /tmp/hashes-before.txt /tmp/hashes-after.txt
    # Should produce no output (files are identical)

### 3.3 Verify manifest is consistent

    diff .aiki/.manifest.json /tmp/manifest-before.json
    # Should produce no output (manifest unchanged)
    # OR timestamps may differ — that's acceptable if checksums are the same

If timestamps changed, verify checksums in the manifest are identical to before.

### 3.4 Verify the template count hasn't changed

    find .aiki/tasks -name '*.md' -not -path '*/tu/*' | wc -l
    # Should still be 12

### 3.5 Record phase 3 results

Note: PASS if re-init is idempotent (no file content changes, no extra or missing templates). FAIL if any template was overwritten, added, or removed.

---

## Phase 4: User Modification Preservation

Test that user-modified templates are NOT overwritten when running `aiki init` again.

### 4.0 Modify a template

Edit `plan.md` to add a custom line at the top of the content (after frontmatter):

    # Save original for later comparison
    cp .aiki/tasks/plan.md /tmp/plan-original.md

Now edit `.aiki/tasks/plan.md` — add a line after the second `---` (end of frontmatter):

```
<!-- CUSTOM: User-added instruction for this repo -->
```

Verify the edit:

    head -10 .aiki/tasks/plan.md
    # Should show frontmatter + the custom line

Record the new hash:

    shasum -a 256 .aiki/tasks/plan.md > /tmp/plan-modified-hash.txt

### 4.1 Run `aiki init` again

    aiki init

### 4.2 Verify the modified template was preserved

    shasum -a 256 .aiki/tasks/plan.md > /tmp/plan-after-init-hash.txt
    diff /tmp/plan-modified-hash.txt /tmp/plan-after-init-hash.txt
    # Should produce no output — the user's modification was preserved

    grep 'CUSTOM: User-added' .aiki/tasks/plan.md
    # Should find the custom line — it was NOT overwritten

### 4.3 Verify unmodified templates are still intact

    shasum -a 256 .aiki/tasks/review/code.md
    # Should match the original manifest checksum (unmodified template left alone)

    shasum -a 256 .aiki/tasks/decompose.md
    # Should also match the original manifest checksum

### 4.4 Record phase 4 results

Note: PASS if user-modified template was preserved through re-init while unmodified templates remain intact. FAIL if the custom line was overwritten or unmodified templates were changed.

---

## Phase 5: Deleted Template Restoration

Test that if a user deletes a template, `aiki init` restores it.

### 5.0 Delete a template

    rm .aiki/tasks/resolve.md
    ls .aiki/tasks/resolve.md 2>&1
    # Should report "No such file or directory"

### 5.1 Run `aiki init`

    aiki init

### 5.2 Verify the deleted template was restored

    ls .aiki/tasks/resolve.md
    # Should exist again

    head -6 .aiki/tasks/resolve.md
    # Should show valid frontmatter (version, type fields)

### 5.3 Verify the restored template matches the source

    # The manifest checksum for resolve.md should match the restored file
    shasum -a 256 .aiki/tasks/resolve.md
    # Compare with the checksum in .manifest.json for "resolve.md"

### 5.4 Verify other templates were not affected

    find .aiki/tasks -name '*.md' -not -path '*/tu/*' | wc -l
    # Should be 12

    # The user-modified plan.md from Phase 4 should still have the custom line
    grep 'CUSTOM: User-added' .aiki/tasks/plan.md
    # Should still find it

### 5.5 Record phase 5 results

Note: PASS if the deleted template was restored with correct content while user-modified templates were preserved. FAIL otherwise.

---

## Phase 6: Template Usability — Can Templates Be Referenced

Test that the installed templates are usable via `aiki run --template`.

### 6.0 List available templates

    aiki template list

**Verify the output includes at least:**
- `plan`
- `decompose`
- `fix`
- `loop`
- `resolve`
- `review/code`
- `review/plan`
- `review/task`
- `explore/code`
- `explore/plan`
- `explore/session`
- `explore/task`

### 6.1 Record phase 6 results

Note: PASS if all 12 templates are listed and resolvable. FAIL if any template is missing from the listing.

---

## Phase 7: Other Init Artifacts

Test that `aiki init` also creates the other expected files alongside templates.

### 7.0 Check hooks.yml

    ls .aiki/hooks.yml
    # Should exist

    cat .aiki/hooks.yml
    # Should contain hook definitions (YAML format)

### 7.1 Check AGENTS.md

    ls AGENTS.md
    # Should exist

    grep '<aiki' AGENTS.md
    # Should find an <aiki> block with task system instructions

### 7.2 Check .gitignore entries

    cat .gitignore

**Verify it includes entries for aiki-specific files:**
- `.aiki/.manifest.json` (or a pattern that covers it)

### 7.3 Record phase 7 results

Note: PASS if hooks.yml, AGENTS.md, and .gitignore are all correctly set up. FAIL otherwise.

---

## Cleanup

Remove the test repo:

    rm -rf /tmp/test-init-templates
    rm -f /tmp/manifest-before.json /tmp/hashes-before.txt /tmp/hashes-after.txt
    rm -f /tmp/plan-original.md /tmp/plan-modified-hash.txt /tmp/plan-after-init-hash.txt

---

## Final Summary

Close the parent task with results:

    aiki task close <PARENT> --summary "Results: Phase 1 (fresh init templates): PASS/FAIL. Phase 2 (manifest correctness): PASS/FAIL. Phase 3 (re-init idempotency): PASS/FAIL. Phase 4 (user modification preservation): PASS/FAIL. Phase 5 (deleted template restoration): PASS/FAIL. Phase 6 (template usability): PASS/FAIL. Phase 7 (other init artifacts): PASS/FAIL. Details: ..."

**Report format:** For each phase and sub-check, state PASS or FAIL with details. Include any error output verbatim. Pay special attention to:
- Were all 12 default templates written to `.aiki/tasks/`?
- Did each template have valid YAML frontmatter with version and type fields?
- Did the manifest accurately track all templates with correct checksums?
- Was re-init idempotent (no unnecessary overwrites)?
- Were user-modified templates preserved through re-init?
- Were deleted templates correctly restored?
- Could templates be listed and referenced by name?
- Were hooks.yml, AGENTS.md, and .gitignore all correctly created?
