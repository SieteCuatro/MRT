#!/bin/bash
# Offscreen X server for scripts/e2e/gui_crash_reattach.py, without root:
#   mkdir -p ~/xvfb-local/debs && cd ~/xvfb-local/debs
#   apt-get download xvfb xserver-common libxfont2 libfontenc1 libxkbfile1 x11-xkb-utils #     xkb-data libpixman-1-0 libxcvt0 x11-apps libunwind8 libxkbcommon-x11-0 libxcb-xkb1
#   for f in *.deb; do dpkg-deb -x "$f" ~/xvfb-local/root; done
#   setsid scripts/e2e/xvfb_offscreen.sh &
# Starts Xvfb :99 inside a user+mount namespace where /usr/bin also has the
# locally extracted xkbcomp (no root, nothing installed system-wide).
R="$HOME/xvfb-local/root"
FAKE="$HOME/xvfb-local/fakebin"
ORIG="$HOME/xvfb-local/origbin"
mkdir -p "$FAKE" "$ORIG"
exec unshare --user --map-root-user --mount bash -c '
  set -e
  mount --bind /usr/bin "'"$ORIG"'"
  rm -rf "'"$FAKE"'"/*
  for f in "'"$ORIG"'"/*; do ln -sf "$f" "'"$FAKE"'/$(basename "$f")"; done
  ln -sf "'"$R"'/usr/bin/xkbcomp" "'"$FAKE"'/xkbcomp"
  mount --bind "'"$FAKE"'" /usr/bin
  export LD_LIBRARY_PATH="'"$R"'/usr/lib/x86_64-linux-gnu"
  exec "'"$R"'/usr/bin/Xvfb" :99 -screen 0 1280x800x24 -xkbdir "'"$R"'/usr/share/X11/xkb" -nolisten tcp -nolisten unix
'
