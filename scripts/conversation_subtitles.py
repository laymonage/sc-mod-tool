#!/usr/bin/env python3
"""Shows subtitles for EnhancedSC's conversation lines that play without any.

Rewrites each matched `Talk(Sound'...', i, , TRUE, p);` in EnhancedSC's
pattern scripts as

    Speech(ConversationSubtitle("<pattern>", "<cue>", "Localization\\<file>"),
           Sound'...', i, 0, TR_CONVERSATION, p, false);

which EnhancedSC's bConversationSubtitles setting turns on and off. The lines
come from the core's dump_unshown_lines example, run on a game folder with
EnhancedSC's packages and localization in system/:

    cargo run -p sc-mod-tool-core --example dump_unshown_lines -- <game dir> lines.json
    python3 scripts/conversation_subtitles.py lines.json <EnhancedSC>/UnrealScript

Calls are found by sound and occurrence, in file order, within the pattern's
state code. Lines without text are left alone, as are calls that aren't latent
or have a delay, which Speech can't reproduce. Everything not rewritten is
listed, so the files can be checked with git diff afterwards.
"""

import argparse
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

TALK = re.compile(
    r"Talk\(\s*Sound'(?P<sound>[\w.]+)'\s*,\s*(?P<index>\d+)\s*"
    r"(?:,\s*(?P<delay>[^,()]*?)\s*"
    r"(?:,\s*(?P<latent>\w+)\s*"
    r"(?:,\s*(?P<progression>[^,()]*?)\s*)?)?)?\)"
)
# Speech calls are dialogue too, so they count towards a sound's occurrences.
SPEECH = re.compile(r"Speech\((?P<converted>ConversationSubtitle\()?[^;]*?Sound'(?P<sound>[\w.]+)'")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("lines", type=Path, help="JSON written by dump_unshown_lines")
    parser.add_argument("unrealscript", type=Path, help="EnhancedSC's UnrealScript folder")
    parser.add_argument("--dry-run", action="store_true", help="report without writing files")
    arguments = parser.parse_args()

    lines = json.loads(arguments.lines.read_text(encoding="utf-8"))["lines"]
    by_pattern = defaultdict(list)
    for line in lines:
        by_pattern[line["pattern"]].append(line)

    classes = arguments.unrealscript / "EchelonPattern" / "Classes"
    skipped = defaultdict(list)
    rewritten = 0
    files = 0
    for pattern, pattern_lines in sorted(by_pattern.items()):
        path = classes / f"{pattern}.uc"
        if not path.is_file():
            for line in pattern_lines:
                skipped["pattern file not found"].append(describe(line))
            continue
        # Latin-1 round-trips every byte, and newline="" keeps line endings.
        with path.open(encoding="latin-1", newline="") as file:
            source = file.read()
        new_source, count = rewrite(source, pattern_lines, skipped)
        if count:
            rewritten += count
            files += 1
            if not arguments.dry_run:
                with path.open("w", encoding="latin-1", newline="") as file:
                    file.write(new_source)

    action = "Would rewrite" if arguments.dry_run else "Rewrote"
    print(f"{action} {rewritten} of {len(lines)} lines in {files} files.")
    for reason, items in sorted(skipped.items(), key=lambda item: -len(item[1])):
        print(f"\nSkipped, {reason}: {len(items)}")
        if reason != "no text":
            for item in items:
                print(f"  {item}")
    return 0


def rewrite(source, pattern_lines, skipped):
    """Rewrites the pattern's matched calls, returning the source and how many."""
    wanted = {}
    for line in pattern_lines:
        if not line["sourceObject"].endswith(".Pattern'"):
            skipped["not in state Pattern"].append(describe(line))
            continue
        wanted[(line["soundEvent"].lower(), line["occurrence"])] = line

    calls = sorted(
        [*code_matches(TALK, source), *code_matches(SPEECH, source)],
        key=lambda match: match.start(),
    )
    occurrences = Counter()
    replacements = []
    found = set()
    for match in calls:
        sound = match["sound"].lower()
        key = (sound, occurrences[sound])
        occurrences[sound] += 1
        line = wanted.get(key)
        if line is None:
            continue
        found.add(key)
        if match.re is SPEECH:
            if match["converted"]:
                skipped["already converted"].append(describe(line))
            else:
                skipped["already a Speech call"].append(describe(line))
            continue
        if line["text"] is None:
            skipped["no text"].append(describe(line))
            continue
        latent = (match["latent"] or "").lower()
        delay = (match["delay"] or "").strip()
        if latent != "true":
            skipped["not latent"].append(describe(line))
            continue
        if delay not in ("", "0", "0.0", "0.000000"):
            skipped["has a delay"].append(describe(line))
            continue
        progression = (match["progression"] or "0").strip() or "0"
        replacements.append(
            (
                match.start(),
                match.end(),
                f'Speech(ConversationSubtitle("{line["pattern"]}", "{line["cue"]}", '
                f'"Localization\\\\{line["localizationFile"]}"), '
                f"Sound'{match['sound']}', {match['index']}, 0, TR_CONVERSATION, "
                f"{progression}, false)",
            )
        )

    for key, line in wanted.items():
        if key not in found:
            skipped["no matching Talk call"].append(describe(line))

    for start, end, text in reversed(replacements):
        source = source[:start] + text + source[end:]
    return source, len(replacements)


def code_matches(pattern, source):
    """Matches of pattern outside // and /* */ comments."""
    comments = [match.span() for match in re.finditer(r"//[^\n]*|/\*.*?\*/", source, re.S)]
    for match in pattern.finditer(source):
        if not any(start <= match.start() < end for start, end in comments):
            yield match


def describe(line):
    return f"{line['pattern']}: {line['soundEvent']} #{line['occurrence']} -> {line['cue']}"


if __name__ == "__main__":
    sys.exit(main())
