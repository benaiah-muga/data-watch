#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if [[ $(uname -s) != Linux || $(uname -m) != x86_64 ]]; then
  echo 'This release script targets Linux x86-64.' >&2
  exit 1
fi
cargo build --release --locked --offline
release_dir=target/release-dist
package_name=data-watch-linux-x86_64
mkdir -p "$release_dir/$package_name"
cp target/release/data-watch "$release_dir/$package_name/data-watch"
chmod 755 "$release_dir/$package_name/data-watch"
cp LICENSE README.md THIRD_PARTY_NOTICES.md "$release_dir/$package_name/"
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C "$release_dir" -cf - "$package_name" | gzip -n > "$release_dir/$package_name.tar.gz"
(cd "$release_dir" && sha256sum "$package_name.tar.gz" > SHA256SUMS)
printf '%s\n' "RELEASE_PACKAGE_READY: $release_dir/$package_name.tar.gz"
