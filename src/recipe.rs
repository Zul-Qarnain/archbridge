use serde::{Deserialize, Serialize};
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
    if go_mod.exists() && check_go_main(dir) {
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
            if content
                .lines()
                .any(|l| l.trim_start().starts_with("install:"))
            {
                return BuildSystem::Make;
            }
        }
    }

    BuildSystem::Unsupported("unsupported build system, manual PKGBUILD required".to_string())
}

fn check_cargo_bin(cargo_path: &Path) -> bool {
    if let Ok(content) = fs::read_to_string(cargo_path) {
        if let Ok(val) = toml::from_str::<toml::Value>(&content) {
            return val.get("bin").is_some();
        }
    }
    false
}

fn check_go_main(dir: &Path) -> bool {
    if dir.join("main.go").exists() {
        return true;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().is_some_and(|ext| ext == "go") {
                if let Ok(content) = fs::read_to_string(&p) {
                    if content.lines().any(|l| l.trim() == "package main") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgbuildParams {
    pub name: String,
    pub version: String,
    pub source_tarball: String,
    pub sha256_hash: String,
    pub dependencies: Vec<String>,
    pub entry_binary: Option<String>,
}

pub fn generate_pkgbuild(system: &BuildSystem, params: &PkgbuildParams) -> Result<String, String> {
    let pkgname = params.name.to_lowercase().replace('_', "-");
    let pkgver = if params.version.is_empty() {
        "1.0.0".to_string()
    } else {
        params.version.replace('-', ".")
    };
    let sha256 = &params.sha256_hash;
    let source_tar = &params.source_tarball;
    let entry = params.entry_binary.as_deref().unwrap_or(&pkgname);

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

    match system {
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
sha256sums=('{sha256}')\n\n\
package() {{\n\
  {extract_cmd}\n\
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
  cd \"$srcdir\"\n\
  cmake -B build -S . -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr\n\
  cmake --build build\n\
}}\n\n\
package() {{\n\
  cd \"$srcdir\"\n\
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
  cd \"$srcdir\"\n\
  make\n\
}}\n\n\
package() {{\n\
  cd \"$srcdir\"\n\
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
  cd \"$srcdir\"\n\
  cargo build --release --locked\n\
}}\n\n\
package() {{\n\
  cd \"$srcdir\"\n\
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
  cd \"$srcdir\"\n\
  go build -mod=readonly -trimpath -o \"{entry}\"\n\
}}\n\n\
package() {{\n\
  cd \"$srcdir\"\n\
  install -Dm755 \"{entry}\" \"$pkgdir/usr/bin/{entry}\"\n\
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
  cd \"$srcdir\"\n\
  npm ci\n\
  npm run build\n\
}}\n\n\
package() {{\n\
  cd \"$srcdir\"\n\
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
  cd \"$srcdir\"\n\
  yarn install --frozen-lockfile\n\
  yarn build\n\
}}\n\n\
package() {{\n\
  cd \"$srcdir\"\n\
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
  cd \"$srcdir\"\n\
  meson setup build --prefix=/usr --wrap-mode=nodownload\n\
  meson compile -C build\n\
}}\n\n\
package() {{\n\
  cd \"$srcdir\"\n\
  DESTDIR=\"$pkgdir\" meson install -C build\n\
}}\n"
        )),
        BuildSystem::Unsupported(reason) => Err(reason.clone()),
    }
}
