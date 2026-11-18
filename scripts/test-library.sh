#!/bin/sh
# Copies testdata/library to a scratch folder, fetches the open-access PDFs and writes a device
# config for it. Prints the command that opens the copy.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
dest=${1:-${TMPDIR:-/tmp}/yeda-test-library}

if [ -d "$dest" ] && [ -n "$(ls -A "$dest" 2>/dev/null)" ]; then
    echo "$dest is not empty; remove it or pass another folder" >&2
    exit 1
fi

mkdir -p "$dest"
cp -R "$root/testdata/library" "$dest/library"

while IFS="$(printf '\t')" read -r key url; do
    case $key in ''|\#*) continue ;; esac
    [ -f "$dest/pdfs/$key/$key.pdf" ] && continue
    mkdir -p "$dest/pdfs/$key"
    echo "fetching $key"
    curl -fsSL --max-time 120 -A "yeda-test-library/0.1 (https://github.com/yuvve/yeda)" \
        -o "$dest/pdfs/$key/$key.pdf" "$url" </dev/null
done < "$root/testdata/pdfs.tsv"

mkdir -p "$dest/config/yeda"
cat > "$dest/config/yeda/config.toml" <<EOF
library = "$dest/library"

[locations]
papers = "$dest/pdfs"
EOF

cat <<EOF
Test library at $dest/library

To open it:

  XDG_CONFIG_HOME="$dest/config" XDG_DATA_HOME="$dest/data" XDG_CACHE_HOME="$dest/cache" \\
    cargo run -p yeda -- "$dest/library"
EOF
