"""Copies the user READMEs of the clients into README.md, between their markers."""

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

HEADING_SHIFT = {"yeda-tui": 0, "yeda-android": 1}


def section(component: str, demote: int) -> str:
    text = (ROOT / component / "README.md").read_text()
    text = text.split("<!-- embed -->\n", 1)[1].split("<!-- /embed -->", 1)[0]
    lines, fenced = [], False
    for line in text.splitlines():
        fenced ^= line.startswith("```")
        if not fenced and line.startswith("#"):
            line = "#" * demote + line
        lines.append(line)
    text = "\n".join(lines).strip()
    return re.sub(r'(\]\(|src=")(?!https?://)', rf"\g<1>{component}/", text)


def main() -> None:
    readme = ROOT / "README.md"
    text = readme.read_text()
    for component, demote in HEADING_SHIFT.items():
        start, end = f"<!-- {component} -->", f"<!-- /{component} -->"
        pattern = re.escape(start) + ".*?" + re.escape(end)
        embedded = f"{start}\n{section(component, demote)}\n{end}"
        text = re.sub(pattern, lambda _: embedded, text, flags=re.S)
    readme.write_text(text)


if __name__ == "__main__":
    main()
