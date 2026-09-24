use crate::archive::FileReview;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BuildSystem {
    Imported,
    Cmake,
    Make,
    Cargo,
    Go,
    Npm,
    Yarn,
    Meson,
    Unsupported(String),
}

pub fn detect_build_system(dir: &Path) -> BuildSystem {
    let cmake = dir.join("CMakeLists.txt");
    let cargo = dir.join("Cargo.toml");
    let go_mod = dir.join("go.mod");
    let meson = dir.join("meson.build");
    let package_json = dir.join("package.json");
    let makefile = dir.join("Makefile");

    if cmake.exists() {
        return BuildSystem::Cmake;
    }
    if cargo.exists() {
        let lock = dir.join("Cargo.lock");
        let has_bin = dir.join("src/main.rs").exists() || check_cargo_bin(&cargo);
        if lock.exists() && has_bin {
            return BuildSystem::Cargo;
        }
    }
    if go_mod.exists() {
        // `go.mod` is authoritative: a module's commands routinely live in
        // `cmd/<name>/` rather than beside the manifest.
        return BuildSystem::Go;
    }
    if meson.exists() {
        return BuildSystem::Meson;
    }
    if package_json.exists() {
        let pkg_lock = dir.join("package-lock.json");
        let yarn_lock = dir.join("yarn.lock");
        if let Ok(content) = fs::read_to_string(&package_json) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                let has_build = val.get("scripts").and_then(|s| s.get("build")).is_some();
                let has_bin = val.get("bin").is_some();
                if has_build && has_bin {
                    if pkg_lock.exists() {
                        return BuildSystem::Npm;
                    }
                    if yarn_lock.exists() {
                        return BuildSystem::Yarn;
                    }
                }
            }
        }
    }
    if makefile.exists() && !dir.join("configure.ac").exists() && !dir.join("configure.in").exists()
    {
        if let Ok(content) = fs::read_to_string(&makefile) {
            if makefile_declares_install(&content) {
                return BuildSystem::Make;
            }
        }
    }

    BuildSystem::Unsupported("unsupported build system, manual PKGBUILD required".to_string())
}

/// The directory every entry of a source archive is nested under, or `None` when
/// the archive is flat. GitHub and GitLab wrap exports in `<repo>-<ref>/`, and
/// only a name makepkg's shell can't reinterpret is usable in a PKGBUILD.
fn archive_root_dir(paths: &[&str]) -> Option<String> {
    let mut root: Option<&str> = None;
    for p in paths.iter().copied() {
        // GitHub materialises the pax extended header as a file beside the tree;
        // it is tar bookkeeping, not a project file, and must not defeat the
        // single-root test.
        if p == "pax_global_header" || p == "." || p == "./" {
            continue;
        }
        if !p.contains('/') {
            return None;
        }
        let first = match p.split('/').next() {
            Some(f) if !f.is_empty() => f,
            _ => return None,
        };
        if !first
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        {
            return None;
        }
        match root {
            None => root = Some(first),
            Some(r) if r == first => {}
            Some(_) => return None,
        }
    }
    root.map(str::to_string)
}

/// Build-system detection for an in-memory archive snapshot.
///
/// `detect_build_system` walks a real directory, but a downloaded source archive
/// carries its top-level directory inside every path, so the tree has to be
/// peeled virtually. Returns the detected system and the directory a generated
/// PKGBUILD must enter before building.
pub fn detect_build_system_in_archive(reviews: &[FileReview]) -> (BuildSystem, Option<String>) {
    let paths: Vec<&str> = reviews.iter().map(|r| r.path.as_str()).collect();
    let root = archive_root_dir(&paths);
    let prefix = root.as_ref().map(|d| format!("{}/", d));

    let mut files: HashMap<&str, &FileReview> = HashMap::new();
    let mut nested: HashSet<&str> = HashSet::new();
    for review in reviews {
        let rel = match &prefix {
            Some(p) => match review.path.strip_prefix(p.as_str()) {
                Some(x) => x,
                None => continue,
            },
            None => review.path.as_str(),
        };
        if rel.is_empty() {
            continue;
        }
        if rel.contains('/') {
            nested.insert(rel);
        } else {
            files.entry(rel).or_insert(review);
        }
    }

    let has = |name: &str| files.contains_key(name);
    let text_of =
        |name: &str| -> Option<&str> { files.get(name).and_then(|r| r.content.as_deref()) };

    if has("CMakeLists.txt") {
        return (BuildSystem::Cmake, root);
    }
    if has("Cargo.toml") {
        let has_bin = nested.contains("src/main.rs")
            || text_of("Cargo.toml").is_some_and(cargo_toml_declares_bin);
        if has("Cargo.lock") && has_bin {
            return (BuildSystem::Cargo, root);
        }
    }
    if has("go.mod") {
        return (BuildSystem::Go, root);
    }
    if has("meson.build") {
        return (BuildSystem::Meson, root);
    }
    if has("package.json") {
        if let Some(content) = text_of("package.json") {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(content) {
                let has_build = val.get("scripts").and_then(|s| s.get("build")).is_some();
                let has_bin = val.get("bin").is_some();
                if has_build && has_bin {
                    if has("package-lock.json") {
                        return (BuildSystem::Npm, root);
                    }
                    if has("yarn.lock") {
                        return (BuildSystem::Yarn, root);
                    }
                }
            }
        }
    }
    if has("Makefile") && !has("configure.ac") && !has("configure.in") {
        if text_of("Makefile").is_some_and(makefile_declares_install) {
            return (BuildSystem::Make, root);
        }
    }

    (
        BuildSystem::Unsupported("unsupported build system, manual PKGBUILD required".to_string()),
        root,
    )
}

fn makefile_declares_install(content: &str) -> bool {
    content
        .lines()
        .any(|l| l.trim_start().starts_with("install:"))
}

fn check_cargo_bin(cargo_path: &Path) -> bool {
    if let Ok(content) = fs::read_to_string(cargo_path) {
        return cargo_toml_declares_bin(&content);
    }
    false
}

fn cargo_toml_declares_bin(content: &str) -> bool {
    matches!(toml::from_str::<toml::Value>(content), Ok(val) if val.get("bin").is_some())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgbuildParams {
    pub name: String,
    pub version: String,
    pub source_tarball: String,
    pub sha256_hash: String,
    pub dependencies: Vec<String>,
    pub entry_binary: Option<String>,
    /// Directory inside `$srcdir` the extracted tarball lands in. Hosted source
    /// archives nest everything under `<repo>-<ref>/`; locally snapshot
    /// directories are flat, so this is `None`.
    pub src_subdir: Option<String>,
}

/// makepkg accepts only `[A-Za-z0-9._@+-]` in `pkgver`, never as the first
/// character, but foreign versions are full of anything else: DEB uses an epoch
/// prefix ("1:0.3.4"), a hyphenated revision ("1.0.0-1") and an upstream tilde
/// ("1.0~rc1"); RPM uses releases ("1.0-2"). Split a numeric epoch into the
/// PKGBUILD `epoch` field so version ordering survives, and normalize every
/// other character makepkg would refuse.
fn normalize_version(raw: &str) -> (String, String) {
    let (epoch, rest) = match raw.split_once(':') {
        Some((prefix, remainder))
            if !prefix.is_empty()
                && !remainder.is_empty()
                && prefix.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (prefix.parse::<u64>().ok(), remainder)
        }
        _ => (None, raw),
    };

    let mut pkgver: String = rest
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '@' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while pkgver.starts_with(|c: char| !c.is_ascii_alphanumeric()) {
        pkgver.remove(0);
    }
    if pkgver.is_empty() {
        pkgver = "1.0.0".to_string();
    }

    let epoch_line = match epoch {
        Some(value) if value > 0 => format!("epoch={}\n", value),
        _ => String::new(),
    };

    (pkgver, epoch_line)
}

/// `pkgname` shares the forbidden character set and additionally may not start
/// with a hyphen, plus sign or period.
fn normalize_name(raw: &str) -> String {
    let mut name: String = raw
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '+' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while name.starts_with(|c| matches!(c, '-' | '+' | '.')) {
        name.remove(0);
    }
    if name.is_empty() {
        name = "imported-package".to_string();
    }
    name
}

/// The entry binary is spliced into shell commands makepkg runs, so a name
/// inferred from a file or directory is rewritten into the character set those
/// commands can hold verbatim. Case is preserved: binary names are case
/// sensitive, unlike `pkgname`.
pub fn sanitize_entry_name(raw: &str) -> String {
    let mut name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '+' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while name.starts_with(|c: char| matches!(c, '-' | '+' | '.')) {
        name.remove(0);
    }
    if name.is_empty() {
        name = "app".to_string();
    }
    name
}

pub fn generate_pkgbuild(system: &BuildSystem, params: &PkgbuildParams) -> Result<String, String> {
    let pkgname = normalize_name(&params.name.replace('_', "-"));
    let (pkgver, epoch_line) = normalize_version(if params.version.is_empty() {
        "1.0.0"
    } else {
        &params.version
    });
    let sha256 = &params.sha256_hash;
    let source_tar = &params.source_tarball;
    let entry = params.entry_binary.as_deref().unwrap_or(&pkgname);
    // The entry name is spliced into shell commands makepkg runs, so anything a
    // quoting layer could reinterpret has to be refused rather than escaped.
    if entry.is_empty()
        || entry.starts_with('-')
        || !entry
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
    {
        return Err(format!(
            "Entry binary name '{}' is not usable in a generated PKGBUILD; pass a plain file name.",
            entry
        ));
    }
    let src_dir = match &params.src_subdir {
        Some(sub) => format!("\"$srcdir/{}\"", sub),
        None => "\"$srcdir\"".to_string(),
    };

    let depends_str = if params.dependencies.is_empty() {
        "()".to_string()
    } else {
        format!(
            "({})",
            params
                .dependencies
                .iter()
                .map(|d| format!("'{}'", d))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };

    let generated = match system {
        BuildSystem::Imported => {
            let extract_cmd = if params.source_tarball.ends_with(".tar.xz")
                || params.source_tarball.ends_with(".tar.zst")
                || params.source_tarball.ends_with(".tar")
            {
                format!(
                    "bsdtar -xf \"$srcdir/{}\" -C \"$pkgdir\"",
                    params.source_tarball
                )
            } else {
                format!(
                    "tar -xzf \"$srcdir/{}\" -C \"$pkgdir\"",
                    params.source_tarball
                )
            };
            Ok(format!(
                "# Maintainer: ArchBridge generated from a foreign package\n\
pkgname='{pkgname}'\n\
pkgver='{pkgver}'\n\
pkgrel=1\n\
pkgdesc='Imported foreign package payload for {pkgname}'\n\
arch=('x86_64')\n\
license=('custom')\n\
depends={depends_str}\n\
options=('!strip')\n\
source=('{source_tar}')\n\
noextract=('{source_tar}')\n\
sha256sums=('{sha256}')\n\n\
package() {{\n\
  {extract_cmd}\n\
  mkdir -p \"$pkgdir/usr/bin\"\n\
  if [ -d \"$pkgdir/usr/share/applications\" ]; then\n\
    for df in \"$pkgdir\"/usr/share/applications/*.desktop; do\n\
      [ -f \"$df\" ] || continue\n\
      exec_bin=$(grep -E \"^Exec=\" \"$df\" | head -n1 | cut -d= -f2- | awk '{{print $1}}')\n\
      exec_bin=$(basename \"$exec_bin\")\n\
      if [ -n \"$exec_bin\" ] && [ ! -e \"$pkgdir/usr/bin/$exec_bin\" ] && [ ! -L \"$pkgdir/usr/bin/$exec_bin\" ]; then\n\
        for cand in \"$pkgdir/opt\"/*\"$exec_bin\" \"$pkgdir/opt\"/*/*\"$exec_bin\"; do\n\
          if [ -f \"$cand\" ] && [ -x \"$cand\" ]; then\n\
            ln -s \"${{cand#$pkgdir}}\" \"$pkgdir/usr/bin/$exec_bin\"\n\
            break\n\
          fi\n\
        done\n\
      fi\n\
    done\n\
  fi\n\
  if [ ! -e \"$pkgdir/usr/bin/{pkgname}\" ] && [ ! -L \"$pkgdir/usr/bin/{pkgname}\" ]; then\n\
    for candidate in \"$pkgdir\"/opt/*/{pkgname} \"$pkgdir\"/opt/*/*; do\n\
      if [ -f \"$candidate\" ] && [ -x \"$candidate\" ]; then\n\
        target_rel=\"${{candidate#$pkgdir}}\"\n\
        ln -s \"$target_rel\" \"$pkgdir/usr/bin/{pkgname}\"\n\
        break\n\
      fi\n\
    done\n\
  fi\n\
}}\n"
            ))
        }
        BuildSystem::Cmake => Ok(format!(
            "# Maintainer: ArchBridge generated\n\
pkgname='{pkgname}'\n\
pkgver='{pkgver}'\n\
pkgrel=1\n\
pkgdesc='Auto-generated PKGBUILD for {pkgname}'\n\
arch=('x86_64')\n\
license=('custom')\n\
depends={depends_str}\n\
makedepends=('cmake' 'ninja' 'gcc')\n\
options=('!debug')\n\
source=('src.tar.gz'::{source_tar})\n\
sha256sums=('{sha256}')\n\n\
build() {{\n\
  cd {src_dir}\n\
  cmake -B build -S . -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr\n\
  cmake --build build\n\
}}\n\n\
package() {{\n\
  cd {src_dir}\n\
  DESTDIR=\"$pkgdir\" cmake --install build\n\
}}\n"
        )),
        BuildSystem::Make => Ok(format!(
            "# Maintainer: ArchBridge generated\n\
pkgname='{pkgname}'\n\
pkgver='{pkgver}'\n\
pkgrel=1\n\
pkgdesc='Auto-generated PKGBUILD for {pkgname}'\n\
arch=('x86_64')\n\
license=('custom')\n\
depends={depends_str}\n\
makedepends=('make' 'gcc')\n\
options=('!debug')\n\
source=('src.tar.gz'::{source_tar})\n\
sha256sums=('{sha256}')\n\n\
build() {{\n\
  cd {src_dir}\n\
  make\n\
}}\n\n\
package() {{\n\
  cd {src_dir}\n\
  make DESTDIR=\"$pkgdir\" PREFIX=/usr install\n\
}}\n"
        )),
        BuildSystem::Cargo => Ok(format!(
            "# Maintainer: ArchBridge generated\n\
pkgname='{pkgname}'\n\
pkgver='{pkgver}'\n\
pkgrel=1\n\
pkgdesc='Auto-generated PKGBUILD for {pkgname}'\n\
arch=('x86_64')\n\
license=('custom')\n\
depends={depends_str}\n\
makedepends=('cargo')\n\
options=('!debug')\n\
source=('src.tar.gz'::{source_tar})\n\
sha256sums=('{sha256}')\n\n\
build() {{\n\
  cd {src_dir}\n\
  cargo build --release --locked\n\
}}\n\n\
package() {{\n\
  cd {src_dir}\n\
  install -Dm755 \"target/release/{entry}\" \"$pkgdir/usr/bin/{entry}\"\n\
}}\n"
        )),
        BuildSystem::Go => Ok(format!(
            "# Maintainer: ArchBridge generated\n\
pkgname='{pkgname}'\n\
pkgver='{pkgver}'\n\
pkgrel=1\n\
pkgdesc='Auto-generated PKGBUILD for {pkgname}'\n\
arch=('x86_64')\n\
license=('custom')\n\
depends={depends_str}\n\
makedepends=('go')\n\
options=('!debug')\n\
source=('src.tar.gz'::{source_tar})\n\
sha256sums=('{sha256}')\n\n\
build() {{\n\
  cd {src_dir}\n\
  go build -trimpath -buildmode=pie -mod=readonly -o \"archbridge-go-bin/\" ./...\n\
}}\n\n\
package() {{\n\
  cd {src_dir}\n\
  if [ ! -f \"archbridge-go-bin/{entry}\" ]; then\n\
    echo \"ArchBridge: the module built no '{entry}' command; pass --entry matching one of: $(ls archbridge-go-bin 2>/dev/null | tr '\\n' ' ')\" >&2\n\
    exit 1\n\
  fi\n\
  install -Dm755 \"archbridge-go-bin/{entry}\" \"$pkgdir/usr/bin/{entry}\"\n\
}}\n"
        )),
        BuildSystem::Npm => Ok(format!(
            "# Maintainer: ArchBridge generated\n\
pkgname='{pkgname}'\n\
pkgver='{pkgver}'\n\
pkgrel=1\n\
pkgdesc='Auto-generated PKGBUILD for {pkgname}'\n\
arch=('x86_64')\n\
license=('custom')\n\
depends={depends_str}\n\
makedepends=('nodejs' 'npm')\n\
options=('!debug')\n\
source=('src.tar.gz'::{source_tar})\n\
sha256sums=('{sha256}')\n\n\
build() {{\n\
  cd {src_dir}\n\
  npm ci\n\
  npm run build\n\
}}\n\n\
package() {{\n\
  cd {src_dir}\n\
  install -d \"$pkgdir/usr/lib/{pkgname}\"\n\
  cp -r . \"$pkgdir/usr/lib/{pkgname}\"\n\
  install -d \"$pkgdir/usr/bin\"\n\
  ln -s \"/usr/lib/{pkgname}/bin/{entry}\" \"$pkgdir/usr/bin/{entry}\"\n\
}}\n"
        )),
        BuildSystem::Yarn => Ok(format!(
            "# Maintainer: ArchBridge generated\n\
pkgname='{pkgname}'\n\
pkgver='{pkgver}'\n\
pkgrel=1\n\
pkgdesc='Auto-generated PKGBUILD for {pkgname}'\n\
arch=('x86_64')\n\
license=('custom')\n\
depends={depends_str}\n\
makedepends=('nodejs' 'yarn')\n\
options=('!debug')\n\
source=('src.tar.gz'::{source_tar})\n\
sha256sums=('{sha256}')\n\n\
build() {{\n\
  cd {src_dir}\n\
  yarn install --frozen-lockfile\n\
  yarn build\n\
}}\n\n\
package() {{\n\
  cd {src_dir}\n\
  install -d \"$pkgdir/usr/lib/{pkgname}\"\n\
  cp -r . \"$pkgdir/usr/lib/{pkgname}\"\n\
  install -d \"$pkgdir/usr/bin\"\n\
  ln -s \"/usr/lib/{pkgname}/bin/{entry}\" \"$pkgdir/usr/bin/{entry}\"\n\
}}\n"
        )),
        BuildSystem::Meson => Ok(format!(
            "# Maintainer: ArchBridge generated\n\
pkgname='{pkgname}'\n\
pkgver='{pkgver}'\n\
pkgrel=1\n\
pkgdesc='Auto-generated PKGBUILD for {pkgname}'\n\
arch=('x86_64')\n\
license=('custom')\n\
depends={depends_str}\n\
makedepends=('meson' 'ninja' 'gcc')\n\
options=('!debug')\n\
source=('src.tar.gz'::{source_tar})\n\
sha256sums=('{sha256}')\n\n\
build() {{\n\
  cd {src_dir}\n\
  meson setup build --prefix=/usr --wrap-mode=nodownload\n\
  meson compile -C build\n\
}}\n\n\
package() {{\n\
  cd {src_dir}\n\
  DESTDIR=\"$pkgdir\" meson install -C build\n\
}}\n"
        )),
        BuildSystem::Unsupported(reason) => Err(reason.clone()),
    }?;

    if epoch_line.is_empty() {
        return Ok(generated);
    }
    // Every template declares `pkgrel=1` exactly once; `epoch` belongs beside it.
    Ok(generated.replacen("pkgrel=1\n", &format!("pkgrel=1\n{}", epoch_line), 1))
}
