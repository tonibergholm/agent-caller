#!/usr/bin/env bash
# Runs INSIDE an archlinux container as root, started by packaging/test-arch.sh.
# It builds both packages with makepkg as a non-root user, installs them,
# runs the smoke test, then checks upgrade, removal and reproducibility.
#
# Inputs (set by test-arch.sh):
#   /in/src.tar            git archive of the repo, top directory agent-caller/
#   ARCH_TEST_OFFLINE=1    pacman is unreachable: skip pacman, build with
#                          `makepkg -d` and a tar source instead of git+https,
#                          using the Rust toolchain in /opt/rust (CARGO_HOME,
#                          RUSTUP_HOME). Everything else is the same.
set -euo pipefail

offline=${ARCH_TEST_OFFLINE:-0}
fails=0
step() { printf '\n== %s\n' "$*"; }
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*"; fails=$((fails + 1)); }
expect() { local d=$1; shift; if "$@" >/dev/null 2>&1; then ok "$d"; else bad "$d"; fi; }

step "container setup (offline=$offline)"
if [ "$offline" != 1 ]; then
	pacman -Syu --noconfirm --needed base-devel git rust shellcheck namcap
fi
id builder >/dev/null 2>&1 || useradd -m builder
as_builder() { runuser -u builder -- env "HOME=/home/builder" "PATH=$PATH" "$@"; }
mkdir -p /work /out && chown builder /work /out
cd /work

# The source the PKGBUILDs build: the repository's HEAD, as a tar (offline)
# or as a tagged git repo answering for the GitHub URL (online).
mkdir -p src && tar -xf /in/src.tar -C src
ver=$(. /pkg/arch/agent-caller/PKGBUILD; echo "$pkgver")
epoch=1700000000
if [ "$offline" != 1 ]; then
	git config --global --add safe.directory '*'
	git -C src/agent-caller init -q -b main
	git -C src/agent-caller add -A
	GIT_AUTHOR_DATE="@$epoch +0000" GIT_COMMITTER_DATE="@$epoch +0000" \
		git -C src/agent-caller -c user.name=t -c user.email=t@example.invalid commit -q -m "v$ver"
	git -C src/agent-caller tag "v$ver"
	git clone -q --bare src/agent-caller /mirror.git
	git config --global url."file:///mirror.git".insteadOf "https://github.com/tonibergholm/agent-caller.git"
	cp /root/.gitconfig /home/builder/.gitconfig && chown builder /home/builder/.gitconfig
else
	cp /in/src.tar /work/agent-caller-src.tar && chown builder /work/agent-caller-src.tar
fi

build() { # build NAME DIR -> packages in /out/DIR
	local name=$1 dir=$2
	rm -rf "/work/$dir" "/out/$dir"
	mkdir -p "/work/$dir" "/out/$dir"
	cp /pkg/arch/"$name"/* "/work/$dir/"
	if [ "$offline" = 1 ]; then
		cp /work/agent-caller-src.tar "/work/$dir/"
		sed -i 's|^source=.*|source=(agent-caller-src.tar)|' "/work/$dir/PKGBUILD"
		# omarchy lists optdepends/source on separate lines; only source is replaced.
	fi
	chown -R builder "/work/$dir" "/out/$dir"
	local flags=(-f --noconfirm)
	[ "$offline" = 1 ] && flags+=(-d)
	( cd "/work/$dir" && as_builder env PKGDEST="/out/$dir" SOURCE_DATE_EPOCH=$epoch \
		${CARGO_HOME:+CARGO_HOME=$CARGO_HOME} ${RUSTUP_HOME:+RUSTUP_HOME=$RUSTUP_HOME} \
		makepkg "${flags[@]}" ) >"/out/$dir.log" 2>&1
}

step "static checks"
if command -v shellcheck >/dev/null; then
	expect "shellcheck smoke-test.sh" shellcheck /pkg/smoke-test.sh
	for f in agent-caller/PKGBUILD agent-caller-omarchy/PKGBUILD; do
		expect "shellcheck $f" shellcheck -s bash -e SC2034,SC2154 /pkg/arch/$f
	done
else
	echo "SKIP  shellcheck not available"
fi

step "build both packages with makepkg as a non-root user"
for p in agent-caller agent-caller-omarchy; do
	if build "$p" "$p-1"; then ok "makepkg $p"; else bad "makepkg $p (see /out/$p-1.log)"; tail -30 "/out/$p-1.log"; fi
done
pk() { ls /out/"$1"-1/"$1"-"$ver"-1-*.pkg.tar.zst 2>/dev/null | head -1; }

step "package contents"
for p in agent-caller agent-caller-omarchy; do
	f=$(pk "$p"); [ -n "$f" ] || { bad "no package file for $p"; continue; }
	echo "-- $p: $f"; bsdtar -tvf "$f" | grep -v '^d.* \.$' | awk '{print $1, $NF}' | grep -v '\.\(BUILDINFO\|MTREE\|PKGINFO\|INSTALL\)' || true
done
if command -v namcap >/dev/null; then
	for p in agent-caller agent-caller-omarchy; do
		namcap /pkg/arch/"$p"/PKGBUILD || true
		namcap "$(pk "$p")" || true
	done
else
	echo "SKIP  namcap not available"
fi
step "binaries are stripped"
for p in agent-caller:acd agent-caller-omarchy:acd-inbox; do
	pkg=${p%%:*}; bin=${p##*:}
	rm -rf /tmp/x && mkdir /tmp/x && bsdtar -xf "$(pk "$pkg")" -C /tmp/x
	if readelf -S /tmp/x/usr/bin/$bin | grep -q '\.symtab\|\.debug_info'; then bad "$bin still has symbols/debug info"; else ok "$bin stripped"; fi
done

step "install both packages (root, pacman -U)"
pacman -U --noconfirm "$(pk agent-caller)" "$(pk agent-caller-omarchy)" && ok "pacman -U" || bad "pacman -U"

step "smoke test as a normal user"
if as_builder bash /usr/share/agent-caller/smoke-test.sh; then ok "smoke test"; else bad "smoke test"; fi

step "smoke test fails on a stale per-user plugin copy, passes when it matches"
id_=tonibergholm.agentcaller
as_builder bash -c "mkdir -p ~/.config/omarchy/plugins/$id_ && cp /usr/share/agent-caller/omarchy/$id_/* ~/.config/omarchy/plugins/$id_/"
if as_builder bash /usr/share/agent-caller/smoke-test.sh >/dev/null; then ok "matching copy passes"; else bad "matching copy should pass"; fi
as_builder bash -c "echo stale >> ~/.config/omarchy/plugins/$id_/Panel.qml"
if as_builder bash /usr/share/agent-caller/smoke-test.sh >/dev/null; then bad "stale copy should fail"; else ok "stale copy fails (exit non-zero)"; fi
as_builder rm -rf /home/builder/.config/omarchy

step "ExecStart of both units under systemd-analyze verify"
for u in acd acd-inbox; do expect "$u.service" as_builder systemd-analyze verify /usr/lib/systemd/user/$u.service; done

step "upgrade and removal keep config and tokens, owner-only"
as_builder bash -c 'acd init >/dev/null 2>&1'
cfg=/home/builder/.config/acd
before=$(cd "$cfg" && find . -type f -o -type d | sort | xargs stat -c '%n %a %U')
sums=$(cd "$cfg" && find . -type f | sort | xargs sha256sum)
if echo "$before" | awk '{ m=$2; if (m !~ /00$/) bad=1 } END { exit bad }'; then ok "all config files and dirs are owner-only before upgrade"; else bad "group/world bits in $cfg"; fi
# pkgrel 2 stands in for the next release.
for p in agent-caller agent-caller-omarchy; do
	rm -rf "/work/$p-2" "/out/$p-2"; mkdir -p "/work/$p-2" "/out/$p-2"
	cp /pkg/arch/$p/* "/work/$p-2/"; sed -i 's/^pkgrel=1/pkgrel=2/' "/work/$p-2/PKGBUILD"
	[ "$offline" = 1 ] && { cp /work/agent-caller-src.tar "/work/$p-2/"; sed -i 's|^source=.*|source=(agent-caller-src.tar)|' "/work/$p-2/PKGBUILD"; }
	chown -R builder "/work/$p-2" "/out/$p-2"
	flags=(-f --noconfirm); [ "$offline" = 1 ] && flags+=(-d)
	( cd "/work/$p-2" && as_builder env PKGDEST="/out/$p-2" SOURCE_DATE_EPOCH=$epoch \
		${CARGO_HOME:+CARGO_HOME=$CARGO_HOME} ${RUSTUP_HOME:+RUSTUP_HOME=$RUSTUP_HOME} makepkg "${flags[@]}" ) >"/out/$p-2.log" 2>&1 \
		&& ok "built $p pkgrel=2" || bad "build $p pkgrel=2"
done
pacman -U --noconfirm /out/agent-caller-2/*.pkg.tar.zst /out/agent-caller-omarchy-2/*.pkg.tar.zst 2>&1 | grep -v '^$' | tail -12
[ "$(pacman -Q agent-caller)" = "agent-caller $ver-2" ] && ok "upgraded to $ver-2" || bad "upgrade did not apply"
after=$(cd "$cfg" && find . -type f -o -type d | sort | xargs stat -c '%n %a %U')
[ "$before" = "$after" ] && ok "config tree and modes unchanged by upgrade" || bad "config tree changed by upgrade"
[ "$sums" = "$(cd "$cfg" && find . -type f | sort | xargs sha256sum)" ] && ok "token and config contents unchanged by upgrade" || bad "contents changed"
pacman -R --noconfirm agent-caller agent-caller-omarchy >/dev/null 2>&1 && ok "pacman -R" || bad "pacman -R"
[ ! -e /usr/bin/acd ] && [ ! -e /usr/bin/acd-inbox ] && [ ! -e /usr/lib/systemd/user/acd.service ] && ok "package files removed" || bad "package files remain"
[ "$before" = "$(cd "$cfg" && find . -type f -o -type d | sort | xargs stat -c '%n %a %U')" ] && [ "$sums" = "$(cd "$cfg" && find . -type f | sort | xargs sha256sum)" ] && ok "config and tokens survive removal, modes unchanged" || bad "removal changed config"

step "reproducibility: same tag, another directory, same bytes"
for p in agent-caller agent-caller-omarchy; do
	build "$p" "$p-3" || { bad "rebuild $p"; continue; }
	a=$(pk "$p"); b=$(ls /out/$p-3/*.pkg.tar.zst)
	bin=acd; [ "$p" = agent-caller-omarchy ] && bin=acd-inbox
	ba=$(bsdtar -xOf "$a" usr/bin/$bin | sha256sum | cut -c1-16)
	bb=$(bsdtar -xOf "$b" usr/bin/$bin | sha256sum | cut -c1-16)
	[ "$ba" = "$bb" ] && ok "$p binary identical ($ba)" || bad "$p binary differs ($ba vs $bb)"
	if [ "$(sha256sum <"$a")" = "$(sha256sum <"$b")" ]; then ok "$p package file identical"; else echo "NOTE  $p package file differs (binary above is the check)"; fi
done

echo
if [ "$fails" -eq 0 ]; then echo "ALL CHECKS PASSED"; else echo "$fails CHECK(S) FAILED"; exit 1; fi
