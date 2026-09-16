set -euo pipefail

if [[ "${ARCHBRIDGE_DISPOSABLE_ARCH_VM:-}" != "yes" ]]; then
  printf '%s\n' 'Refusing: run only inside a disposable Arch VM and set ARCHBRIDGE_DISPOSABLE_ARCH_VM=yes.' >&2
  exit 2
fi

binary="${ARCHBRIDGE_BINARY:-target/debug/archbridge}"
if [[ ! -x "$binary" ]]; then
  printf '%s\n' 'Build ArchBridge first with cargo build.' >&2
  exit 2
fi

"$binary" doctor
"$binary" search bash
"$binary" search yay
"$binary" search archbridge-upstream-probe --repo https://github.com/ninja-build/ninja
"$binary" build https://github.com/ninja-build/ninja --name archbridge-ninja --entry ninja --dry-run
"$binary" build https://github.com/sharkdp/hexyl --name archbridge-hexyl --entry hexyl --dry-run
"$binary" build https://github.com/jarun/nnn --name archbridge-nnn --entry nnn --dependency ncurses --dependency readline --dry-run

if [[ "${ARCHBRIDGE_RUN_BUILDS:-}" != "yes" ]]; then
  printf '%s\n' 'Plans only. Review them, then set ARCHBRIDGE_RUN_BUILDS=yes to execute builds/tests. No live install is requested.'
  exit 0
fi

sudo -v
"$binary" build https://github.com/ninja-build/ninja --name archbridge-ninja --entry ninja --yes
"$binary" build https://github.com/sharkdp/hexyl --name archbridge-hexyl --entry hexyl --yes
"$binary" build https://github.com/jarun/nnn --name archbridge-nnn --entry nnn --dependency ncurses --dependency readline --yes