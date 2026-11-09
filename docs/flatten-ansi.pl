#!/usr/bin/env perl
# Rewrite SGR sequences from `tmux capture-pane -e` into absolute truecolor
# with reverse video applied, for renderers (freeze) that ignore 39/49/7.
# Palette: Catppuccin Mocha.
use strict;
use warnings;

my @pal = qw(
    11111b f38ba8 a6e3a1 f9e2af 89b4fa f5c2e7 94e2d5 bac2de
    7f849c f38ba8 a6e3a1 f9e2af 89b4fa f5c2e7 94e2d5 a6adc8
);
my ($dfg, $dbg) = ('cdd6f4', '1e1e2e');
my ($fg, $bg, $bold, $rev) = (undef, undef, 0, 0);

sub rgb { join ';', map { hex } ($_[0] =~ /(..)(..)(..)/) }

sub emit {
    my ($f, $b) = ($fg // $dfg, $bg // $dbg);
    ($f, $b) = ($b, $f) if $rev;
    return "\e[0" . ($bold ? ';1' : '') . ';38;2;' . rgb($f) . ';48;2;' . rgb($b) . 'm';
}

while (my $line = <STDIN>) {
    $line =~ s{\e\[([0-9;]*)m}{
        my @p = split /;/, ($1 eq '' ? '0' : $1);
        while (@p) {
            my $c = shift @p;
            if    ($c == 0)  { ($fg, $bg, $bold, $rev) = (undef, undef, 0, 0) }
            elsif ($c == 1)  { $bold = 1 }
            elsif ($c == 22) { $bold = 0 }
            elsif ($c == 7)  { $rev = 1 }
            elsif ($c == 27) { $rev = 0 }
            elsif ($c == 39) { $fg = undef }
            elsif ($c == 49) { $bg = undef }
            elsif ($c == 38 || $c == 48) {
                my $m = shift @p;
                my $col;
                if ($m == 5) {
                    my $n = shift @p;
                    $col = $n < 16 ? $pal[$n] : 'cdd6f4';
                } elsif ($m == 2) {
                    $col = sprintf '%02x%02x%02x', splice(@p, 0, 3);
                }
                if ($c == 38) { $fg = $col } else { $bg = $col }
            }
            elsif ($c >= 30 && $c <= 37)   { $fg = $pal[$c - 30] }
            elsif ($c >= 90 && $c <= 97)   { $fg = $pal[$c - 82] }
            elsif ($c >= 40 && $c <= 47)   { $bg = $pal[$c - 40] }
            elsif ($c >= 100 && $c <= 107) { $bg = $pal[$c - 92] }
        }
        emit();
    }ge;
    print $line;
}
