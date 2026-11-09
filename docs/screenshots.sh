#!/usr/bin/env bash
# Regenerate the README screenshots from `upst --demo`.
#
#   nix shell nixpkgs#tmux nixpkgs#charm-freeze nixpkgs#librsvg nixpkgs#perl -c docs/screenshots.sh
#
# Runs the demo in a private tmux server for about four minutes so the
# live part of the charts has an outage in it, captures panes with their
# colors, and renders them with freeze and rsvg-convert. freeze ignores
# the "default color" and reverse-video escape codes, so flatten-ansi.pl
# rewrites them first, and the font is passed by name (freeze hangs on
# --font.file) and must have braille glyphs and a 0.6 em advance, like
# Adwaita Mono.
set -euo pipefail

cd "$(dirname "$0")/.."
cargo build --release
bin=$PWD/target/release/upst
docs=$PWD/docs
work=$(mktemp -d)
trap 'tmux -L upst-shots kill-server 2>/dev/null || true; rm -rf "$work"' EXIT
font=${UPST_SHOT_FONT:-Adwaita Mono}
styles="battery lcd segments blocks"

t() { tmux -L upst-shots -f /dev/null "$@"; }
cap() { t capture-pane -p -e -t s > "$work/$1.ans"; echo "captured $1"; }

# One narrow, tall demo per charge style; only their Battery cards are kept.
for style in $styles; do
    t new-session -d -s "$style" -x 60 -y 80 "env TERM=xterm-256color $bin --config /dev/null --demo --charge-style $style"
done
t new-session -d -s s -x 128 -y 38 "env TERM=xterm-256color $bin --config /dev/null --demo"
# The rack UPS loses power about 20 s in and hits low battery near 115 s.
sleep 30
for style in $styles; do
    t capture-pane -p -e -t "$style" | perl -CS -Mutf8 -e '
        my @l = <STDIN>;
        my @plain = map { (my $p = $_) =~ s/\e\[[0-9;]*m//g; $p } @l;
        my ($a) = grep { $plain[$_] =~ /^\x{256d} Battery/ } 0..$#l;
        my ($b) = grep { $_ > $a && $plain[$_] =~ /^\x{2570}/ } 0..$#l;
        print @l[$a..$b];' > "$work/style-$style.ans"
    t kill-session -t "$style"
    echo "captured style-$style"
done
sleep 68
cap overview
t send-keys -t s ']'; sleep 105
t send-keys -t s '['; sleep 1
t send-keys -t s '2'; sleep 0.5; t send-keys -t s '-'; sleep 1.5; cap history
t send-keys -t s '3'; sleep 0.5; t send-keys -t s j j; sleep 1.5; cap variables
t send-keys -t s '1'; sleep 0.5; t send-keys -t s c; sleep 0.5
t send-keys -t s G k k k k k Enter; sleep 1; cap confirm
t send-keys -t s Escape; sleep 0.5
t resize-window -t s -x 44 -y 9; sleep 1.5; cap compact
t send-keys -t s q

for name in overview history variables confirm compact $(printf 'style-%s ' $styles); do
    perl "$docs/flatten-ansi.pl" < "$work/$name.ans" > "$work/$name.flat"
    freeze "$work/$name.flat" -l ansi -o "$work/$name.svg" --window=false --padding 24 \
        --background "#1e1e2e" --border.radius 8 --font.family "$font" --font.size 15 < /dev/null > /dev/null
    rsvg-convert -z 1.5 "$work/$name.svg" -o "$docs/$name.png"
    echo "wrote docs/$name.png"
done
