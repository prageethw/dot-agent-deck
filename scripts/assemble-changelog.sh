#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?Usage: assemble-changelog.sh <version>}"
CHANGELOG_DIR="changelog.d"
CHANGELOG_FILE="CHANGELOG.md"
DATE=$(date +%Y-%m-%d)

# Collect entries by type. The five recognized suffixes are the semantic
# fragment names declared in pyproject.toml's [[tool.towncrier.type]] table
# (feature, bugfix, breaking, doc, misc) — see the vocabulary check below.
declare -A TYPE_HEADERS=(
  [feature]="Added"
  [breaking]="Changed"
  [bugfix]="Fixed"
  [doc]="Documentation"
  [misc]="Miscellaneous"
)

# Ordered list of types to scan — earlier entries appear first in the changelog.
# `breaking` is deliberately first so Breaking Changes leads the release notes;
# this order is independent of pyproject.toml's declaration order (see below).
TYPES=(breaking feature bugfix doc misc)

# pyproject.toml is the single declared source of the valid type vocabulary
# (docs/develop/versioning.md, R5) even though this script — not towncrier
# itself — is what actually assembles the changelog (C5, deliberate: towncrier
# is never invoked). Read only the SET of `directory = "..."` values from its
# [[tool.towncrier.type]] blocks; pyproject.toml's own TYPES order and its
# `name` fields (Features/Bug Fixes/…) are NOT authoritative here — taking
# either would silently reorder or re-title every future changelog.
PYPROJECT_TOML="pyproject.toml"
pyproject_types=()
while IFS= read -r t; do
  [[ -n "$t" ]] && pyproject_types+=("$t")
done < <(awk '
  /^\[\[tool\.towncrier\.type\]\]/ { in_block=1; next }
  /^\[/ { in_block=0 }
  in_block && /^[[:space:]]*directory[[:space:]]*=/ {
    line=$0
    sub(/^[[:space:]]*directory[[:space:]]*=[[:space:]]*/, "", line)
    gsub(/["'"'"']/, "", line)
    gsub(/[[:space:]]+$/, "", line)
    print line
  }
' "$PYPROJECT_TOML" 2>/dev/null)

if [ ${#pyproject_types[@]} -eq 0 ]; then
  echo "ERROR: could not parse any [[tool.towncrier.type]] directory entries from $PYPROJECT_TOML" >&2
  echo "A silently empty vocabulary would turn the unknown-suffix check below into a no-op that accepts everything — refusing to proceed instead." >&2
  exit 1
fi

# Assert the script's TYPES and pyproject.toml's declared types are the same
# set, so the two vocabularies can never silently diverge again (PRD fork#340,
# C4: pyproject.toml, this script and analyze.sh disagreeing let a fragment
# type pass here and hard-fail the release later).
missing_in_script=()
for t in "${pyproject_types[@]}"; do
  found=false
  for s in "${TYPES[@]}"; do
    [[ "$s" == "$t" ]] && { found=true; break; }
  done
  $found || missing_in_script+=("$t")
done

extra_in_script=()
for s in "${TYPES[@]}"; do
  found=false
  for t in "${pyproject_types[@]}"; do
    [[ "$t" == "$s" ]] && { found=true; break; }
  done
  $found || extra_in_script+=("$s")
done

if [ ${#missing_in_script[@]} -gt 0 ] || [ ${#extra_in_script[@]} -gt 0 ]; then
  echo "ERROR: changelog type vocabulary mismatch between $PYPROJECT_TOML and $0" >&2
  if [ ${#missing_in_script[@]} -gt 0 ]; then
    echo "  pyproject.toml declares a type this script has no TYPES/TYPE_HEADERS entry for:" >&2
    printf '    %s\n' "${missing_in_script[@]}" >&2
  fi
  if [ ${#extra_in_script[@]} -gt 0 ]; then
    echo "  this script recognizes a type pyproject.toml does not declare:" >&2
    printf '    %s\n' "${extra_in_script[@]}" >&2
  fi
  echo "Keep TYPES/TYPE_HEADERS in $0 and [[tool.towncrier.type]] in $PYPROJECT_TOML in sync." >&2
  exit 1
fi

# Fail loudly if changelog.d/ contains fragments with unrecognized suffixes,
# rather than silently skipping them. (v0.24.3 shipped with `*.fix.md` fragments
# that were ignored because only `*.bugfix.md` is recognized, leaving the
# GitHub release body and CHANGELOG.md empty for that version.)
#
# LOCKSTEP: this validation `find` and the collection `find` further down MUST
# scan the same tree. Both are recursive — no `-maxdepth` on either — so every
# `*.md` under changelog.d/ is suffix-checked before the collection loop can
# reach it, and nothing is deleted without having passed this gate. They
# disagreed once: validation was `-maxdepth 1` while collection was unbounded,
# so a fragment in a subdirectory was invisible to the guard and visible to the
# `rm -f` at the end, and an unrecognized one sat there while its siblings were
# consumed around it (issue #582) — the same silent-drop shape this guard was
# written to prevent, reached by the one path it did not cover. Change the depth
# of one and you must change the other.
if [ -d "$CHANGELOG_DIR" ]; then
  unknown_fragments=()
  while IFS= read -r -d '' f; do
    name="$(basename "$f")"
    [[ "$name" == ".gitkeep" ]] && continue
    matched=false
    for type in "${TYPES[@]}"; do
      [[ "$name" == *.${type}.md ]] && { matched=true; break; }
    done
    # Report the path relative to changelog.d/, so a fragment in a
    # subdirectory names the directory to look in; for the flat layout the
    # repo actually uses this is still just the filename.
    $matched || unknown_fragments+=("${f#"$CHANGELOG_DIR"/}")
  done < <(find "$CHANGELOG_DIR" -name '*.md' -print0 2>/dev/null)

  if [ ${#unknown_fragments[@]} -gt 0 ]; then
    echo "ERROR: changelog.d/ contains fragments with unrecognized type suffix:" >&2
    printf '  %s\n' "${unknown_fragments[@]}" >&2
    echo >&2
    echo "Recognized types: ${TYPES[*]}" >&2
    echo "Rename each fragment so its suffix matches one of the recognized types (e.g. '.bugfix.md', '.feature.md')." >&2
    exit 1
  fi
fi

section=""
processed_files=()
seen_headers=()

for type in "${TYPES[@]}"; do
  fragments=()
  # LOCKSTEP with the validation `find` above — see the note there. Recursive,
  # and every path it can yield has already been suffix-checked by the time this
  # loop runs, which is what makes the `rm -f` at the end safe.
  while IFS= read -r -d '' f; do
    fragments+=("$f")
  done < <(find "$CHANGELOG_DIR" -name "*.$type.md" -print0 2>/dev/null | sort -z)

  if [ ${#fragments[@]} -gt 0 ]; then
    header="${TYPE_HEADERS[$type]}"
    # Deduplicate headers (each of the five types maps to a distinct header
    # today, but this stays defensive against a future TYPE_HEADERS collision)
    if [[ ! " ${seen_headers[*]:-} " =~ " ${header} " ]]; then
      section+="### ${header}"$'\n\n'
      seen_headers+=("$header")
    fi
    for f in "${fragments[@]}"; do
      processed_files+=("$f")
      while IFS= read -r line; do
        # Skip blank lines
        [[ -z "$line" ]] && continue
        # Convert markdown headings to bold list items
        if [[ "$line" =~ ^##\ (.+) ]]; then
          section+="- **${BASH_REMATCH[1]}**"$'\n'
        else
          section+="  $line"$'\n'
        fi
      done < "$f"
    done
    section+=$'\n'
  fi
done

if [ -z "$section" ]; then
  echo "No changelog fragments found in $CHANGELOG_DIR/" >&2
  exit 0
fi

# Build the new release section
release_section="## [$VERSION] - $DATE"$'\n\n'"$section"

# Output to stdout (used by release workflow for release notes)
echo "$release_section"

# Prepend to CHANGELOG.md, preserving the header if present
if [ -f "$CHANGELOG_FILE" ]; then
  first_line=$(head -n 1 "$CHANGELOG_FILE")
  if [[ "$first_line" =~ ^#\ Changelog ]]; then
    rest=$(tail -n +2 "$CHANGELOG_FILE" | sed '/./,$!d')
    printf '%s\n\n%s\n\n%s\n' "$first_line" "$release_section" "$rest" > "$CHANGELOG_FILE"
  else
    existing=$(cat "$CHANGELOG_FILE")
    printf '%s\n\n%s\n' "$release_section" "$existing" > "$CHANGELOG_FILE"
  fi
else
  printf '# Changelog\n\n%s\n' "$release_section" > "$CHANGELOG_FILE"
fi

# Remove only processed fragments (keep .gitkeep and unprocessed files)
for f in "${processed_files[@]}"; do
  rm -f "$f"
done
