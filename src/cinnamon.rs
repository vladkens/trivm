// Copyright (c) vladkens | MIT License | https://github.com/vladkens/trivm
//! Linux Mint's desktop for arm64, which Mint builds for amd64 only: Mint 22.3's own source
//! packages of Cinnamon, built in the guest on Ubuntu 24.04, the base Mint builds them on, and
//! installed with Mint's packages for any architecture; the packages built are kept in the cache
//! for the next VM.

use std::path::PathBuf;

use crate::vm;

/// Mint 22.3's sources of Cinnamon from its repository, which has no HTTPS (the SHA-256 checks
/// them), in the order they build in: each needs some of those before it installed.
pub const SOURCES: &[(&str, &str)] = &[
  (
    "http://packages.linuxmint.com/pool/backport/c/cinnamon-desktop/cinnamon-desktop_6.6.2+zena.tar.xz",
    "5398f0e5532a4bda568b1d2e6fe6ce70109577df7d3675b6f66a95717e2224e1",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/c/cinnamon-menus/cinnamon-menus_6.6.0+zena.tar.xz",
    "ddb2603e46b26c2f29d927c3d0c079880d6cbf14c93362fa31de85d80c7ac333",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/x/xapp/xapp_3.2.4+mint22.3.tar.xz",
    "274c2332ba458b3f722485785b10ef08d416286b2486c20585b40a4183b27fb7",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/c/cjs/cjs_115.2+mint22.3.tar.xz",
    "fab640cb2cad02d0da050a0d1c6660bf369c209694f6b257020b9504b0df56b0",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/m/muffin/muffin_6.6.3+zena.tar.xz",
    "a1277afc27d39dc5da38d2241c3ff4f78c8bdb9937ddb4082f2782e8a599f5b3",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/c/cinnamon-session/cinnamon-session_6.6.3+zena.tar.xz",
    "b61a1cb83225850af7ba2df9d4d5072c46c02eb243022dc58d61aa1577dd13c1",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/c/cinnamon-settings-daemon/cinnamon-settings-daemon_6.6.4+zena.tar.xz",
    "77cafc4a694a1e4aa82abda7434021e4208d23dd15800653174f74b504882166",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/c/cinnamon-control-center/cinnamon-control-center_6.6.0+zena.tar.xz",
    "8564bbeacaf0466bc4349877ff39a82bbf78b7293973a234e9c6bb6a62ec31cd",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/c/cinnamon-screensaver/cinnamon-screensaver_6.6.3+mint22.3.tar.xz",
    "3838130fdfe414b8a802f9b6c4c3c900029f691e40efafc25551913b80d0379f",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/n/nemo/nemo_6.6.3+zena.tar.xz",
    "fafcef183ebb43ffbd1d536b9daf21554c5cd2f23538ad75bb170b4e66edc42e",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/c/cinnamon/cinnamon_6.6.9+zena.tar.xz",
    "8bc3f44a30c2ccc44ec65bda9684e24b2ad0aba8f557b66ee25e03ae9f9b402d",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/s/slick-greeter/slick-greeter_2.2.6+zena.tar.xz",
    "9a9d6f659d91746e27f39612c66a747781618ac5da42f19866888d210a80d242",
  ),
];

/// Mint's packages for any architecture that Cinnamon needs and Ubuntu 24.04 lacks or has older
/// (python3-xapp), and Mint's artwork: its themes, icons, cursors, background and defaults.
pub const ALL: &[(&str, &str)] = &[
  (
    "http://packages.linuxmint.com/pool/main/f/flags/iso-flag-png_1.0.4_all.deb",
    "4bef7ca2c6756baba587ba3766996c423e114efb512e8b3af0e1c31be7dbd6dd",
  ),
  (
    "http://packages.linuxmint.com/pool/main/m/mint-artwork/mint-artwork_1.9.3_all.deb",
    "ed1e44dc159a282ab56ff61bb373e13ea6017cb4a1875f40cb5ca7ba34ec75ba",
  ),
  (
    "http://packages.linuxmint.com/pool/main/m/mint-cursor-themes/mint-cursor-themes_1.0.2_all.deb",
    "a0058085cdf5bcedf9698c91efa82a36179b95ef83af2a64b67639a5066cf020",
  ),
  (
    "http://packages.linuxmint.com/pool/main/m/mint-themes/mint-themes_2.3.8_all.deb",
    "7c5b71f855ec0bda74c3fa86be40a1e4d897dada1a057e3654773e38e47c93fe",
  ),
  (
    "http://packages.linuxmint.com/pool/main/m/mint-x-icons/mint-x-icons_1.7.5_all.deb",
    "217b54fb4a28630cf655799e2c95b1dfd6f4536c4977ca70724fd0b703f4e08e",
  ),
  (
    "http://packages.linuxmint.com/pool/main/m/mint-y-icons/mint-y-icons_1.9.1_all.deb",
    "56e4785ba55b14553d65a71d5821beee569b62fa47ac35b021d8edb65c0714e4",
  ),
  (
    "http://packages.linuxmint.com/pool/main/p/python3-xapp/python3-xapp_3.0.3_all.deb",
    "43318df62cbb3bb02b22876fc457d73d0027144d1c838a3013875ff8438b7c3b",
  ),
  (
    "http://packages.linuxmint.com/pool/main/x/xapp-symbolic-icons/xapp-symbolic-icons_1.0.9_all.deb",
    "dd99f740af5f4d1c374f3116748ec8e8c6ca791d2ce666e711d7f735a1b52ba5",
  ),
  (
    "http://packages.linuxmint.com/pool/backport/c/cinnamon-translations/cinnamon-l10n_6.6.2_all.deb",
    "7faa8c1e15033239717ed378984a5dbff58668fe0ba2a91cd1846c88b729bdef",
  ),
];

/// Where the guest builds, and leaves the packages built for the host to keep.
pub const BUILT: &str = "/var/tmp/trivm-mint";

/// Builds the sources in `$1/src` in order, each with what it needs and those before it installed,
/// unless `$1/debs` holds the packages built for an earlier VM; installs them with `$1/all`, then
/// removes what only the build needed (its tools, `-dev`, `-doc`, `-dbg`): both ways end alike.
const BUILD: &str = r#"set -e
export DEBIAN_FRONTEND=noninteractive NEEDRESTART_SUSPEND=1
seed=$1 out=/var/tmp/trivm-mint
mkdir -p $out/debs
apt-mark showmanual | sort > $out/manual
names() { for x in "$@"; do [ -e "$x" ] || continue; x=${x##*/}; echo ${x%%_*}; done; }
# Mint's cursors are made the default by an alternative in this folder, which a desktop has.
mkdir -p /usr/share/icons/default
if [ -d $seed/debs ]; then
  cp $seed/debs/*.deb $out/debs/
else
  export DEB_BUILD_PROFILES='nocheck nodoc' DEB_BUILD_OPTIONS="nocheck nodoc parallel=$(nproc)"
  apt-get install -y --no-install-recommends build-essential dpkg-dev
  apt-get install -y $seed/all/*.deb
  for x in {sources}; do
    mkdir $out/$x
    tar -xJf $seed/src/${x}_*.tar.xz -C $out/$x --strip-components 1
    (cd $out/$x && apt-get build-dep -y -P nocheck,nodoc ./ && dpkg-buildpackage -b -uc -us)
    rm -rf $out/$x $out/*-dbg_*.deb
    apt-get install -y $out/*.deb
  done
  apt-get purge -y $(names $out/*-dev_*.deb $out/*-doc_*.deb)
  rm -f $out/*-dev_*.deb $out/*-doc_*.deb
  mv $out/*.deb $out/debs/
fi
apt-get install -y $out/debs/*.deb $seed/all/*.deb
apt-mark showmanual | sort | comm -13 $out/manual - | xargs -r apt-mark auto
apt-mark manual $(names $out/debs/*.deb $seed/all/*.deb)
apt-get autoremove --purge -y
rm $out/manual
"#;

/// The packages built in the cache, named by a hash of the sources' (FNV-1a), so other sources
/// build anew.
pub fn cached() -> PathBuf {
  let hash = SOURCES.iter().flat_map(|x| x.1.bytes());
  let hash =
    hash.fold(0xcbf2_9ce4_8422_2325_u64, |h, x| (h ^ x as u64).wrapping_mul(0x100_0000_01b3));
  vm::cache_dir().join(format!("mint-cinnamon-{hash:016x}"))
}

/// The source packages' names, in the order they build in.
fn names() -> Vec<&'static str> {
  let name = |url: &'static str| url.rsplit('/').next().and_then(|x| x.split('_').next());
  SOURCES.iter().filter_map(|x| name(x.0)).collect()
}

/// The script that builds and installs Cinnamon in the guest, as root, from the seed's folder.
pub fn script() -> String {
  BUILD.replace("{sources}", &names().join(" "))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn packages() {
    let names = names();
    assert_eq!(names.len(), SOURCES.len());
    assert_eq!(names.first(), Some(&"cinnamon-desktop"));
    assert_eq!(names[names.len() - 2..], ["cinnamon", "slick-greeter"]);
    for (url, sha256) in SOURCES.iter().chain(ALL) {
      assert!(url.starts_with("http://packages.linuxmint.com/pool/"), "{url}");
      assert!(sha256.len() == 64 && sha256.chars().all(|x| x.is_ascii_hexdigit()), "{url}");
    }

    assert!(SOURCES.iter().all(|x| x.0.ends_with(".tar.xz")));
    assert!(ALL.iter().all(|x| x.0.ends_with("_all.deb")));
    let script = script();
    assert!(script.contains("for x in cinnamon-desktop cinnamon-menus xapp cjs muffin "));
    let sh = std::process::Command::new("sh").args(["-n", "-c", &script]).status().unwrap();
    assert!(sh.success());
  }
}
