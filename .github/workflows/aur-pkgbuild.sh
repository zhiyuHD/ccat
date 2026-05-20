cat > PKGBUILD << 'PKGEOF'
# Maintainer: Zhiyu Wang <zhiyuxl@outlook.com>
# Maintainer: LuoTianyi_arm64 <2153585992@qq.com>
# Contributor: Crush <crush@ccat.dev>

pkgname=ccat-rs
PKGEOF
echo "pkgver=$1" >> PKGBUILD
echo "pkgrel=1" >> PKGBUILD
cat >> PKGBUILD << 'PKGEOF'
pkgdesc="Universal file viewer — markdown, docx, images, PDF, audio, video, JSON, YAML, logs, ELF disasm, diff, gz, archives"
arch=('x86_64' 'aarch64')
url="https://github.com/zhiyuHD/ccat"
license=('MIT')
depends=('gcc-libs' 'oniguruma')
makedepends=('cargo' 'pkg-config')
PKGEOF
echo "source=(\"ccat-rs-$1.tar.gz::https://github.com/zhiyuHD/ccat/releases/download/v$1/ccat-rs-$1.tar.gz\")" >> PKGBUILD
echo "sha256sums=('$2')" >> PKGBUILD
cat >> PKGBUILD << 'PKGEOF'

build() {
  cd "$srcdir/$pkgname-$pkgver"
  export CARGO_BUILD_JOBS=$(nproc)
  export RUSTONIG_SYSTEM_LIBONIG=1
  cargo build --release
}

check() {
  cd "$srcdir/$pkgname-$pkgver"
  export RUSTONIG_SYSTEM_LIBONIG=1
  cargo test --release --frozen
}

package() {
  cd "$srcdir/$pkgname-$pkgver"
  install -Dm755 target/release/ccat "$pkgdir/usr/bin/ccat"
  install -Dm644 completions/ccat.bash "$pkgdir/usr/share/bash-completion/completions/ccat"
  install -Dm644 completions/_ccat "$pkgdir/usr/share/zsh/site-functions/_ccat"
  install -Dm644 completions/ccat.fish "$pkgdir/usr/share/fish/vendor_completions.d/ccat.fish"
  install -Dm644 ccat.1 "$pkgdir/usr/share/man/man1/ccat.1"
}
PKGEOF
